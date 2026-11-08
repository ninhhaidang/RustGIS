//! Model providers: building the HTTP request for a conversation and reading the reply, for the
//! four wire formats the assistant speaks (OpenAI Chat Completions, which OpenRouter, Ollama and
//! most local servers also speak; OpenAI Responses, which the ChatGPT sign-in uses; Anthropic
//! Messages; Gemini `generateContent`).
//!
//! Nothing here does I/O: a request is plain data ([`HttpRequest`]) that the desktop sends on a
//! thread and the browser sends with its `fetch`.

use crate::Error;
use rustgis_engine::data::secrets::{Secret, SecretError, SecretStore};
use serde_json::{Map, Value, json};

/// A wire format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Api {
    /// `POST {base}/chat/completions`.
    OpenAi,
    /// `POST {base}/responses` (streamed): the OpenAI Responses API, as a ChatGPT plan's sign-in uses it.
    Responses,
    /// `POST {base}/v1/messages`.
    Anthropic,
    /// `POST {base}/models/{model}:generateContent`.
    Gemini,
}

/// A provider the settings offer, with its defaults.
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub api: Api,
    pub base_url: &'static str,
    pub model: &'static str,
    /// Environment variables an API key is read from (desktop and command line), in order.
    pub key_env: &'static [&'static str],
    /// An environment variable that overrides the base URL.
    pub url_env: &'static str,
    /// Whether it works without a key.
    pub keyless: bool,
    /// Where to get a key.
    pub key_url: &'static str,
    /// Signs in with an account (see [`crate::chatgpt`]) instead of taking a key: the key is the
    /// account's access token, kept by the sign-in, never typed or saved with the preferences.
    pub login: bool,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "anthropic",
        label: "Anthropic",
        api: Api::Anthropic,
        base_url: "https://api.anthropic.com",
        model: "claude-opus-5-5",
        key_env: &["ANTHROPIC_API_KEY"],
        url_env: "",
        keyless: false,
        key_url: "https://console.anthropic.com/settings/keys",
        login: false,
    },
    Preset {
        id: "openai",
        label: "OpenAI",
        api: Api::OpenAi,
        base_url: "https://api.openai.com/v1",
        model: "gpt-5.5",
        key_env: &["OPENAI_API_KEY"],
        url_env: "",
        keyless: false,
        key_url: "https://platform.openai.com/api-keys",
        login: false,
    },
    Preset {
        id: "chatgpt",
        label: "ChatGPT (sign in)",
        api: Api::Responses,
        base_url: crate::chatgpt::API,
        // OpenAI's example model; the automatically loaded account catalog says
        // which it may use.
        model: "gpt-6.1-sol",
        key_env: &[],
        url_env: "",
        keyless: false,
        key_url: crate::chatgpt::USAGE_URL,
        login: true,
    },
    Preset {
        id: "gemini",
        label: "Google Gemini",
        api: Api::Gemini,
        base_url: "https://generativelanguage.googleapis.com/v1beta",
        model: "gemini-3.8-flash",
        key_env: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        url_env: "",
        keyless: false,
        key_url: "https://aistudio.google.com/apikey",
        login: false,
    },
    Preset {
        id: "openrouter",
        label: "OpenRouter",
        api: Api::OpenAi,
        base_url: "https://openrouter.ai/api/v1",
        model: "openrouter/auto",
        key_env: &["OPENROUTER_API_KEY"],
        url_env: "",
        keyless: false,
        key_url: "https://openrouter.ai/keys",
        login: false,
    },
    Preset {
        id: "ollama",
        label: "Ollama (local)",
        api: Api::OpenAi,
        base_url: "http://localhost:11434/v1",
        model: "qwen3",
        key_env: &[],
        url_env: "OLLAMA_BASE_URL",
        keyless: true,
        key_url: "https://ollama.com/download",
        login: false,
    },
    Preset {
        id: "custom",
        label: "OpenAI-compatible",
        api: Api::OpenAi,
        base_url: "http://localhost:1234/v1",
        model: "",
        key_env: &["OPENAI_COMPATIBLE_API_KEY"],
        url_env: "OPENAI_COMPATIBLE_BASE_URL",
        keyless: true,
        key_url: "",
        login: false,
    },
    Preset {
        id: "requesty",
        label: "Requesty",
        api: Api::OpenAi,
        base_url: "https://router.requesty.ai/v1",
        model: "anthropic/claude-sonnet-5-5",
        key_env: &["REQUESTY_API_KEY"],
        url_env: "",
        keyless: false,
        key_url: "https://app.requesty.ai/api-keys",
        login: false,
    },
    Preset {
        id: "trustedrouter",
        label: "TrustedRouter",
        api: Api::OpenAi,
        base_url: "https://api.trustedrouter.com/v1",
        model: "trustedrouter/auto",
        key_env: &["TRUSTEDROUTER_API_KEY"],
        url_env: "",
        keyless: false,
        key_url: "https://trustedrouter.com",
        login: false,
    },
];

/// The preset with this id.
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Which model to call, where and with what key.
#[derive(Clone, PartialEq)]
pub struct Config {
    /// A [`Preset`] id.
    pub provider: String,
    pub base_url: String,
    pub model: String,
    /// The API key, or the access token of a signed-in account.
    pub key: String,
    /// The signed-in ChatGPT account (its id token's subject); empty for other providers.
    pub account: String,
    /// The selected registration, including when signed out.
    pub client_id: String,
    /// The sign-in session of a signed-in account.
    pub session: String,
    /// Whether the catalog offers reasoning for the selected model (`Some(false)` also turns it off
    /// for a Chat Completions model that refused it alongside tools: see [`refuses_reasoning_with_tools`]).
    pub reasoning: Option<bool>,
    /// Requested tool output token estimate (default 10,000); the 24,000-token hard limit still applies.
    pub tool_output_token_limit: usize,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config").field("provider", &self.provider).field("model", &self.model).finish_non_exhaustive()
    }
}

impl Config {
    /// The preset's defaults, with a key and base URL from the environment where set. A sign-in
    /// provider's account is not read here: its tokens are in the system keychain, which may block
    /// (`chatgpt::load_any`, then [`Config::sign_in`]).
    pub fn from_env(provider: &str) -> Config {
        let p = preset(provider).unwrap_or(&PRESETS[0]);
        let env = |name: &str| std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let key = p.key_env.iter().find_map(|k| env(k)).unwrap_or_default();
        let mut base_url = if p.url_env.is_empty() { None } else { env(p.url_env) }.unwrap_or_else(|| p.base_url.to_string());
        // `OLLAMA_BASE_URL` names the server, not its OpenAI-compatible API.
        if p.id == "ollama" && !base_url.trim_end_matches('/').ends_with("/v1") {
            base_url = format!("{}/v1", base_url.trim_end_matches('/'));
        }
        Config {
            provider: p.id.to_string(),
            base_url,
            model: p.model.to_string(),
            key,
            account: String::new(),
            client_id: String::new(),
            session: String::new(),
            reasoning: None,
            tool_output_token_limit: env("RUSTGIS_ASSISTANT_TOOL_OUTPUT_TOKEN_LIMIT")
                .and_then(|s| s.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(crate::tools::DEFAULT_OUTPUT_TOKENS),
        }
    }

    /// Uses a signed-in account. The key only says that it is signed in, never a token:
    /// `chatgpt::send` puts the current access token on each request.
    pub fn sign_in(&mut self, c: &crate::chatgpt::Credentials) {
        self.key = if c.plan_allowed() && c.signed_in() { crate::chatgpt::SIGNED_IN.to_string() } else { String::new() };
        self.client_id = c.client_id.clone();
        self.account = c.subject.clone();
        self.session = c.session.clone();
    }

    /// The ChatGPT registration this configuration's requests are for.
    pub fn binding(&self) -> crate::chatgpt::Binding {
        crate::chatgpt::Binding { client_id: self.client_id.clone(), subject: self.account.clone(), session: self.session.clone() }
    }

    /// The provider wire format.
    pub fn api(&self) -> Api {
        preset(&self.provider).map_or(Api::OpenAi, |p| p.api)
    }

    /// Whether the provider signs in with an account rather than a key.
    pub fn login(&self) -> bool {
        preset(&self.provider).is_some_and(|p| p.login)
    }

    /// Why this configuration can't be used yet, if it can't.
    pub fn missing(&self) -> Option<&'static str> {
        let keyless = preset(&self.provider).is_some_and(|p| p.keyless);
        if self.model.trim().is_empty() {
            Some("Choose a model.")
        } else if self.base_url.trim().is_empty() {
            Some("Enter the provider's address.")
        } else if self.key.trim().is_empty() && self.login() {
            Some("Sign in with your ChatGPT account.")
        } else if self.key.trim().is_empty() && !keyless {
            Some("Enter an API key.")
        } else {
            None
        }
    }

    fn base(&self) -> &str {
        self.base_url.trim().trim_end_matches('/')
    }
}

/// The desktop preference naming the provider it uses (`prefs.json`).
pub const PROVIDER_PREF: &str = "assistant_provider";

/// The desktop preference that is "on" when it keeps API keys in the system keychain.
pub const REMEMBER_KEY_PREF: &str = "assistant_remember_key";

/// The desktop preference holding a provider's address, when it isn't the default.
pub fn url_pref(provider: &str) -> String {
    format!("assistant_url_{provider}")
}

/// The value [`REMEMBER_KEY_PREF`] is saved with.
pub fn remember_key_value(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Whether a saved [`REMEMBER_KEY_PREF`] value asks to keep keys in the keychain.
pub fn remembers_keys(value: Option<&str>) -> bool {
    value == Some("on")
}

/// The provider the desktop app chose (its [`PROVIDER_PREF`]), when it is one that exists.
pub fn saved_provider(prefs: &Map<String, Value>) -> Option<&'static str> {
    prefs.get(PROVIDER_PREF).and_then(Value::as_str).and_then(preset).map(|p| p.id)
}

/// The first key-using provider whose API key `config` finds (the environment's, for
/// `Config::from_env`). A sign-in is chosen, not assumed.
pub fn first_keyed(config: impl Fn(&str) -> Config) -> Option<&'static str> {
    PRESETS.iter().find(|p| !p.login && !config(p.id).key.is_empty()).map(|p| p.id)
}

/// The provider a command-line run uses: the explicit choice, then the environment's choice, then
/// the first provider with an environment key, then (only when `saved` has the desktop's
/// preferences) the one the desktop app uses, else `ollama`. Environment credentials win over the
/// desktop's saved choice, as every environment setting wins over a saved one.
pub fn choose(explicit: Option<&str>, environment: Option<&str>, keyed: Option<&str>, saved: Option<&Map<String, Value>>) -> String {
    explicit.or(environment).or(keyed).or_else(|| saved.and_then(saved_provider)).unwrap_or("ollama").to_string()
}

/// The keychain account of a provider's API key (the desktop's Remember).
pub fn key_account(provider: &str) -> String {
    format!("assistant/{provider}")
}

/// Reads the desktop's remembered API key only for its provider and address, when no key was
/// supplied by the environment. Sign-in providers never use API-key accounts.
pub fn saved_key(cfg: &Config, prefs: &Map<String, Value>, store: &dyn SecretStore) -> Result<Option<Secret>, SecretError> {
    if cfg.login() || !cfg.key.is_empty() || !remembers_keys(prefs.get(REMEMBER_KEY_PREF).and_then(Value::as_str)) {
        return Ok(None);
    }
    let desktop = prefs
        .get(&url_pref(&cfg.provider))
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Config::from_env(&cfg.provider).base_url);
    if desktop.trim().trim_end_matches('/') != cfg.base_url.trim().trim_end_matches('/') {
        return Ok(None);
    }
    store.load(&key_account(&cfg.provider)).map(|key| key.filter(|key| !key.expose().trim().is_empty()))
}

/// An HTTP request to make.
#[derive(Clone, PartialEq)]
pub struct HttpRequest {
    /// The HTTP method.
    pub method: &'static str,
    /// The destination; may contain secrets.
    pub url: String,
    /// Request headers; may contain credentials.
    pub headers: Vec<(String, String)>,
    /// The request payload; never logged by Debug.
    pub body: Option<String>,
}

/// The response headers the assistant reads and keeps: retry hints and request ids. No others
/// are stored (cookies and the like never are).
pub const KEPT_HEADERS: [&str; 6] =
    ["retry-after", "retry-after-ms", "x-request-id", "openai-request-id", "x-oai-request-id", "request-id"];

/// What came back.
#[derive(Clone, PartialEq)]
pub struct HttpResponse {
    /// The HTTP status.
    pub status: u16,
    /// The response payload; may contain credentials.
    pub body: Vec<u8>,
    /// The response headers in [`KEPT_HEADERS`] (names in lower case).
    pub headers: Vec<(String, String)>,
}

impl HttpResponse {
    /// A header's value (`name` in lower case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The provider's id for this request, for support requests.
    pub fn request_id(&self) -> Option<&str> {
        ["x-request-id", "openai-request-id", "x-oai-request-id", "request-id"].into_iter().find_map(|h| self.header(h))
    }
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest").field("method", &self.method).finish_non_exhaustive()
    }
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpResponse").field("status", &self.status).field("request_id", &self.request_id()).finish_non_exhaustive()
    }
}

/// How long to wait before sending a failed request again, if it is worth retrying: rate limits
/// and overloaded or failing servers (429, 408, 5xx, 529) and connections that failed, never a
/// usage limit or an exhausted quota (those wait for the user). The server's `retry-after` is
/// honoured up to two minutes; otherwise 2, 4, 8… seconds after `attempt` earlier retries.
pub fn retry_delay(resp: &Result<HttpResponse, crate::Error>, attempt: u32) -> Option<core::time::Duration> {
    use core::time::Duration;
    let backoff = Duration::from_secs(2u64.saturating_pow(attempt.saturating_add(1)).min(30));
    let r = match resp {
        Ok(r) => r,
        // The diagnostic text is independent of transport classification.
        Err(e) => return (e.transport == Some(crate::error::TransportFailure::Connection)).then_some(backoff),
    };
    if !matches!(r.status, 408 | 429 | 500 | 502 | 503 | 504 | 529) {
        return None;
    }
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    let code = body.pointer("/error/code").or_else(|| body.pointer("/error/type")).and_then(Value::as_str).unwrap_or("");
    if matches!(
        code,
        "insufficient_quota"
            | "billing_hard_limit_reached"
            | "subscription_sharing_usage_limit_exceeded"
            | "subscription_sharing_user_not_eligible"
            | "usage_limit_reached"
    ) {
        return None;
    }
    // Seconds past the cap mean "not soon": no retry. Converted only once known to fit, so a
    // hostile value (1e30) can't overflow a Duration.
    let cap = Duration::from_secs(120);
    let seconds = |s: f64| if s > cap.as_secs_f64() { cap + Duration::from_secs(1) } else { Duration::from_secs_f64(s) };
    let told = r.header("retry-after-ms").and_then(|v| v.trim().parse::<u64>().ok()).map(Duration::from_millis).or_else(|| {
        let v = r.header("retry-after")?.trim();
        match v.parse::<f64>() {
            Ok(s) if s.is_finite() && s >= 0.0 => Some(seconds(s)),
            Ok(_) => None,
            // An HTTP date (`Wed, 21 Oct 2015 07:28:00 GMT`): the wait until then.
            Err(_) => http_date_wait(v).map(seconds),
        }
    });
    match told {
        Some(d) if d > cap => None,
        Some(d) => Some(d),
        None => Some(backoff),
    }
}

/// HTTP-200 streams can report overload too. No tools from this failed response have run;
/// discard the response and its preview before replaying the same request. Unknown interruptions
/// and quota/authentication errors are deliberately not replayed.
pub fn retry_delay_for(
    resp: &Result<HttpResponse, crate::Error>,
    parsed: &Result<Reply, crate::Error>,
    attempt: u32,
) -> Option<core::time::Duration> {
    if let Some(delay) = retry_delay(resp, attempt) {
        return Some(delay);
    }
    let r = resp.as_ref().ok().filter(|r| (200..300).contains(&r.status))?;
    let error = match parsed {
        Err(e) => e,
        Ok(reply) => reply.failure.as_ref()?,
    };
    if !matches!(error.code.as_deref(), Some("overloaded_error" | "rate_limit_error" | "rate_limit_exceeded" | "server_error")) {
        return None;
    }
    retry_delay(&Ok(HttpResponse { status: 503, body: Vec::new(), headers: r.headers.clone() }), attempt)
}

/// Seconds from now until an HTTP date (IMF-fixdate, `Wed, 21 Oct 2015 07:28:00 GMT`), 0 when it
/// has passed. In the browser, which has no clock here, a date counts as "not soon".
fn http_date_wait(v: &str) -> Option<f64> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    // `<weekday>, <day> <month> <year> <hh:mm:ss> GMT`
    let mut p = v.split_whitespace().skip(1);
    let day: i64 = p.next()?.parse().ok()?;
    let name = p.next()?;
    let month = MONTHS.iter().position(|m| *m == name)? as i64 + 1;
    let year: i64 = p.next()?.parse().ok()?;
    let mut hms = p.next()?.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi, s) = (hms.next()??, hms.next()??, hms.next()??);
    // Bounded, so the arithmetic below can't overflow on a hostile header.
    if p.next()? != "GMT"
        || !(1..=31).contains(&day)
        || !(1970..=9999).contains(&year)
        || !(0..=23).contains(&h)
        || !(0..=59).contains(&mi)
        || !(0..=60).contains(&s)
    {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let at = era.checked_mul(146_097)?.checked_add(doe - 719_468)?.checked_mul(86_400)?.checked_add(h * 3600 + mi * 60 + s)?;
    Some(date_delay(at, now_unix()))
}

fn date_delay(at: i64, now: Option<i64>) -> f64 {
    // A valid server date cannot safely become a short retry when no clock is available.
    now.map_or(f64::INFINITY, |now| at.saturating_sub(now).max(0) as f64)
}

#[cfg(test)]
mod retry_clock_tests {
    #[test]
    fn missing_clock_never_shortens_a_valid_http_date_wait() {
        assert!(super::date_delay(1000, None).is_infinite());
        assert_eq!(super::date_delay(1000, Some(998)), 2.0);
        assert_eq!(super::date_delay(1000, Some(1002)), 0.0);
        assert!(super::date_delay(1000, Some(1)) > 120.0);
    }
}

/// Seconds since 1970 now, where there is a clock to read (not in the browser).
fn now_unix() -> Option<i64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().and_then(|d| i64::try_from(d.as_secs()).ok())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// The largest reply read.
pub const MAX_RESPONSE: usize = 8 << 20;

/// The largest streamed reply read (an event stream repeats the reply in its deltas and, for a
/// ChatGPT plan, carries the encrypted reasoning too).
pub const MAX_STREAM: usize = 64 << 20;

/// The largest reply to read for `req`: [`MAX_STREAM`] when it asks for an event stream.
pub fn reply_limit(req: &HttpRequest) -> usize {
    let stream = req.headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("accept") && v.contains("text/event-stream"));
    if stream { MAX_STREAM } else { MAX_RESPONSE }
}

/// Whether the assistant may send a request (with an API key) to `url`: HTTPS anywhere, plain
/// HTTP only to this computer or the local network (Ollama, LM Studio).
pub fn allowed_url(url: &str) -> bool {
    let url = url.trim();
    let (https, rest) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(r), _) => (true, r),
        (None, Some(r)) => (false, r),
        _ => return false,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // No credentials in the address (they would be echoed in errors), and
    // `http://localhost@evil.example` is not local.
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    if https {
        return true;
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    }
    .to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") || host == "::1" {
        return true;
    }
    // `010.0.0.1` is octal to the system's resolver (8.0.0.1), not 10.0.0.1.
    if host.split('.').any(|p| p.len() > 1 && p.starts_with('0')) {
        return false;
    }
    let octets: Vec<u8> = host.split('.').filter_map(|p| p.parse().ok()).collect();
    if octets.len() != 4 || host.split('.').count() != 4 {
        return false;
    }
    match (octets.first(), octets.get(1)) {
        (Some(127), _) | (Some(10), _) => true,
        (Some(192), Some(168)) => true,
        (Some(172), Some(b)) => (16..=31).contains(b),
        _ => false,
    }
}

/// A tool the model may call.
#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// A JSON Schema object (`{"type": "object", "properties": …}`).
    pub schema: Value,
}

/// A call the model made.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

/// What a call gave back.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub id: String,
    pub name: String,
    pub content: String,
    pub error: bool,
}

/// One turn of the conversation, in no provider's format.
#[derive(Clone, Debug)]
pub enum Turn {
    User(String),
    Assistant {
        text: String,
        calls: Vec<ToolCall>,
        /// The reply as the provider sent it, sent back unchanged to the same provider: it may carry
        /// what the model needs to see again (Claude's thinking blocks, Gemini's thought signatures).
        native: Option<(Api, Value)>,
    },
    /// The results of the calls in the assistant turn before.
    Results(Vec<ToolResult>),
}

/// A message from RustGIS in the transcript (a stop, a retry, a compaction): an English template
/// naming its values in braces, which the interface translates before filling them in
/// (`i18n::fmt`); its `Display` is the English text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub template: &'static str,
    pub args: Vec<(&'static str, String)>,
}

impl Note {
    pub fn new(template: &'static str) -> Self {
        Note { template, args: Vec::new() }
    }

    /// A template with values, e.g. `Note::with("The reply stopped early ({reason}).", [("reason", why)])`.
    pub fn with<const N: usize>(template: &'static str, args: [(&'static str, String); N]) -> Self {
        Note { template, args: args.into() }
    }

    /// Text RustGIS doesn't write itself (a diagnostic), shown as it is.
    pub fn text(text: String) -> Self {
        Note::with("{text}", [("text", text)])
    }
}

/// Every [`Note`] template RustGIS writes (the interface's catalogs translate each).
pub const NOTE_TEMPLATES: &[&str] = &[
    "The reply reached its length limit.",
    "The provider's content filter stopped the reply.",
    "The reply stopped early ({reason}).",
    "The model declined this request.",
    "The model declined this request: {why}",
    "The reply stopped before completion; no actions were run.",
    "Gemini stopped the reply ({reason}).",
    "Summarising the earlier conversation to keep it within the model's context…",
    "Compacted the earlier conversation.",
    "The provider returned HTTP {status}; trying again in {seconds} s.",
    "The connection failed; trying again in {seconds} s.",
    "The model sent an empty reply; ask again, or try another model.",
    "Stopped after {steps} steps; say \"continue\" to go on.",
    "Stopped.",
];

/// The note of a refused reply, with the model's explanation if it gave one.
fn declined(why: String) -> Note {
    if why.trim().is_empty() {
        Note::new("The model declined this request.")
    } else {
        Note::with("The model declined this request: {why}", [("why", why)])
    }
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut rest = self.template;
        while let Some((before, tail)) = rest.split_once('{') {
            f.write_str(before)?;
            let Some((key, after)) = tail.split_once('}') else {
                f.write_str("{")?;
                return f.write_str(tail);
            };
            if let Some((_, value)) = self.args.iter().find(|(name, _)| *name == key) {
                f.write_str(value)?;
            } else {
                write!(f, "{{{key}}}")?;
            }
            rest = after;
        }
        f.write_str(rest)
    }
}

/// A reply read from the provider.
#[derive(Clone, Debug)]
pub struct Reply {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub native: Value,
    /// Why the reply stopped early (length limit, a refusal), if it did.
    pub cut: Option<Note>,
    /// A failed terminal response accompanying partial text.
    pub failure: Option<Error>,
}

/// Replies may be long (tables, explanations) but are never documents.
const MAX_TOKENS: u32 = 16_000;

/// The request asking the model for its next turn.
pub fn chat_request(cfg: &Config, system: &str, turns: &[Turn], tools: &[ToolDef]) -> HttpRequest {
    match cfg.api() {
        Api::OpenAi => openai_request(cfg, system, turns, tools),
        Api::Responses => responses_request(cfg, system, turns, tools),
        Api::Anthropic => anthropic_request(cfg, system, turns, tools),
        Api::Gemini => gemini_request(cfg, system, turns, tools),
    }
}

/// Reads a chat reply (any status).
pub fn parse_reply(api: Api, resp: &HttpResponse) -> Result<Reply, Error> {
    parse_reply_inner(api, resp).map_err(|mut e| {
        e.status = Some(resp.status);
        e.request_id = e.request_id.or_else(|| resp.request_id().map(str::to_string));
        if let Some(detail) = e.detail.strip_prefix(&format!("HTTP {}: ", resp.status)) {
            e.detail = detail.to_string();
        }
        e
    })
}

fn parse_reply_inner(api: Api, resp: &HttpResponse) -> Result<Reply, Error> {
    let parse = match api {
        Api::Responses => return responses_reply(resp),
        Api::OpenAi => openai_reply,
        Api::Anthropic => anthropic_reply,
        Api::Gemini => gemini_reply,
    };
    let v = assembled(api, &resp.body).ok_or_else(|| http_error(resp))?;
    if !(200..300).contains(&resp.status) || v.get("error").is_some_and(|e| !e.is_null()) {
        let mut e = Error::from(error_message(&v).unwrap_or_else(|| http_error(resp)));
        // The provider's code (context_length_exceeded, overloaded_error…) and request id stay.
        e.code =
            ["/error/code", "/error/type", "/error/status"].iter().find_map(|p| v.pointer(p).and_then(Value::as_str)).map(str::to_string);
        e.param = v.pointer("/error/param").and_then(Value::as_str).map(str::to_string);
        e.status = Some(resp.status);
        e.request_id = resp.request_id().map(str::to_string);
        return Err(e);
    }
    parse(&v).map_err(Error::from)
}

// ---------------------------------------------------------------------------------------------
// Streaming: replies come as server-sent events. The whole stream is read before its calls run
// (approval needs complete calls); meanwhile [`StreamPreview`] shows the text as it arrives.

/// A streamed reply reassembled into the JSON the API sends without streaming, so one parser
/// reads both; a plain JSON body (a server that didn't stream) is read as it is.
fn assembled(api: Api, body: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return serde_json::from_str(trimmed).ok();
    }
    let events = sse_events(&text);
    if events.is_empty() {
        return None;
    }
    // `"error": null` (some compatible servers send it in every chunk) is no error.
    if let Some(e) = events.iter().find_map(|e| e.get("error").filter(|e| !e.is_null())) {
        return Some(json!({"error": e}));
    }
    // A connection closed mid-reply still reads cleanly: without its end the reply is partial, and
    // its calls may be missing or cut short. Not replayed (the code isn't a retryable one).
    if !stream_ended(api, &text, &events) {
        return Some(
            json!({"error": {"code": "incomplete_stream", "message": "The reply stopped before it finished, so nothing in it was run."}}),
        );
    }
    Some(match api {
        Api::OpenAi => openai_assembled(&events),
        Api::Anthropic => anthropic_assembled(&events),
        Api::Gemini => gemini_assembled(&events),
        // Read event by event by its own parser.
        Api::Responses => return None,
    })
}

/// Whether a stream reached its end: `[DONE]` or a finish reason (Chat Completions), the
/// message's stop (Anthropic), a finish reason (Gemini). Responses streams are checked by their
/// own parser.
fn stream_ended(api: Api, text: &str, events: &[Value]) -> bool {
    let set = |v: Option<&Value>| v.is_some_and(|v| !v.is_null());
    match api {
        Api::OpenAi => {
            text.lines().any(|l| l.strip_prefix("data:").map(str::trim) == Some("[DONE]"))
                || events
                    .iter()
                    .any(|e| e.get("choices").and_then(Value::as_array).into_iter().flatten().any(|c| set(c.get("finish_reason"))))
        }
        Api::Anthropic => {
            events.iter().any(|e| e.get("type").and_then(Value::as_str) == Some("message_stop") || set(e.pointer("/delta/stop_reason")))
        }
        Api::Gemini => {
            events.iter().any(|e| e.get("candidates").and_then(Value::as_array).into_iter().flatten().any(|c| set(c.get("finishReason"))))
        }
        Api::Responses => true,
    }
}

/// Chat Completions chunks: content and reasoning deltas, and calls whose arguments come in
/// pieces, by index.
fn openai_assembled(events: &[Value]) -> Value {
    let (mut content, mut reasoning, mut finish) = (String::new(), String::new(), Value::Null);
    let mut calls: Vec<(String, String, String)> = Vec::new();
    for e in events {
        for choice in e.get("choices").and_then(Value::as_array).into_iter().flatten() {
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            content.push_str(delta.get("content").and_then(Value::as_str).unwrap_or(""));
            reasoning.push_str(delta.get("reasoning_content").and_then(Value::as_str).unwrap_or(""));
            for tc in delta.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
                let id = tc.get("id").and_then(Value::as_str).filter(|id| !id.is_empty());
                // By its index; without one (some local servers), a new id starts a new call.
                let i = match tc.get("index").and_then(Value::as_u64) {
                    Some(i) => usize::try_from(i).unwrap_or(0).min(255),
                    None => match (id, calls.last()) {
                        // A call whose id comes after its first piece is still that call.
                        (Some(id), Some(last)) if !last.0.is_empty() && last.0 != id => calls.len(),
                        (_, Some(_)) => calls.len() - 1,
                        (_, None) => 0,
                    },
                };
                while calls.len() <= i {
                    calls.push(Default::default());
                }
                if let Some(c) = calls.get_mut(i) {
                    if let Some(id) = id {
                        c.0 = id.to_string();
                    }
                    // The name comes once (some servers repeat it in every piece).
                    if let Some(name) = tc.pointer("/function/name").and_then(Value::as_str).filter(|_| c.1.is_empty()) {
                        c.1.push_str(name);
                    }
                    c.2.push_str(tc.pointer("/function/arguments").and_then(Value::as_str).unwrap_or(""));
                }
            }
            if let Some(f) = choice.get("finish_reason").filter(|f| !f.is_null()) {
                finish = f.clone();
            }
        }
    }
    let mut message = json!({"role": "assistant", "content": if content.is_empty() { Value::Null } else { json!(content) }});
    // Slots no piece named (a stray index) are no call.
    calls.retain(|c| !c.1.is_empty());
    if !calls.is_empty() {
        message["tool_calls"] = calls
            .into_iter()
            .map(|(id, name, args)| json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}}))
            .collect();
    }
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    json!({"choices": [{"finish_reason": finish, "message": message}]})
}

/// Anthropic events: content blocks started, extended by deltas (text, tool input JSON,
/// thinking and its signature) and stopped; the stop reason comes last.
fn anthropic_assembled(events: &[Value]) -> Value {
    let mut blocks: Vec<Value> = Vec::new();
    let mut inputs: Vec<String> = Vec::new();
    let mut message = json!({});
    let index = |e: &Value| e.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).filter(|i| *i < 256);
    for e in events {
        match e.get("type").and_then(Value::as_str) {
            // Hostile input: a message or block that isn't an object is ignored (indexing a
            // non-object would panic).
            Some("message_start") => message = e.get("message").filter(|m| m.is_object()).cloned().unwrap_or_else(|| json!({})),
            Some("content_block_start") => {
                if let (Some(i), Some(b)) = (index(e), e.get("content_block").filter(|b| b.is_object())) {
                    while blocks.len() <= i {
                        blocks.push(Value::Null);
                        inputs.push(String::new());
                    }
                    if let Some(slot) = blocks.get_mut(i) {
                        *slot = b.clone();
                    }
                }
            }
            Some("content_block_delta") => {
                let (Some(i), Some(d)) = (index(e), e.get("delta")) else { continue };
                let Some(b) = blocks.get_mut(i).and_then(Value::as_object_mut) else { continue };
                let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                match d.get("type").and_then(Value::as_str) {
                    Some("text_delta") => append(b, "text", &s("text")),
                    Some("thinking_delta") => append(b, "thinking", &s("thinking")),
                    Some("signature_delta") => {
                        b.insert("signature".into(), json!(s("signature")));
                    }
                    Some("input_json_delta") => {
                        if let Some(input) = inputs.get_mut(i) {
                            input.push_str(&s("partial_json"));
                        }
                    }
                    _ => {}
                }
            }
            Some("message_delta") => {
                for (k, v) in e.get("delta").and_then(Value::as_object).into_iter().flatten() {
                    message[k] = v.clone();
                }
            }
            _ => {}
        }
    }
    for (b, input) in blocks.iter_mut().zip(&inputs) {
        if let Some(b) = b.as_object_mut().filter(|_| !input.is_empty()) {
            b.insert("input".into(), serde_json::from_str(input).unwrap_or_else(|_| json!({"_unparsed": input})));
        }
    }
    blocks.retain(|b| !b.is_null());
    message["content"] = Value::Array(blocks);
    message
}

fn append(o: &mut Map<String, Value>, key: &str, more: &str) {
    match o.get_mut(key) {
        Some(Value::String(text)) => text.push_str(more),
        _ => {
            o.insert(key.into(), json!(more));
        }
    }
}

/// Gemini chunks: each a response with the next parts; the finish reason comes last.
fn gemini_assembled(events: &[Value]) -> Value {
    let mut parts: Vec<Value> = Vec::new();
    let mut finish = Value::Null;
    let mut feedback = Value::Null;
    for e in events {
        if let Some(f) = e.get("promptFeedback") {
            feedback = f.clone();
        }
        let Some(c) = e.get("candidates").and_then(|c| c.get(0)) else { continue };
        parts.extend(c.pointer("/content/parts").and_then(Value::as_array).cloned().unwrap_or_default());
        if let Some(f) = c.get("finishReason") {
            finish = f.clone();
        }
    }
    if parts.is_empty() && finish.is_null() {
        return json!({"promptFeedback": feedback});
    }
    json!({"candidates": [{"content": {"role": "model", "parts": parts}, "finishReason": finish}]})
}

/// A streamed reply's text so far, for showing while it arrives (what runs is read from the
/// whole reply). Fed the body as it comes, in pieces of any size.
#[derive(Debug, Clone)]
pub struct StreamPreview {
    api: Api,
    pending: Vec<u8>,
    scanned: usize,
    text: String,
}

impl StreamPreview {
    pub fn new(api: Api) -> Self {
        StreamPreview { api, pending: Vec::new(), scanned: 0, text: String::new() }
    }

    /// The text so far.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Takes the next piece of the body; says whether the text grew.
    pub fn feed(&mut self, chunk: &[u8]) -> bool {
        let before = self.text.len();
        self.pending.extend_from_slice(chunk);
        let mut consumed = 0;
        let mut scan = self.scanned.min(self.pending.len());
        while let Some(offset) = self.pending.get(scan..).and_then(|p| p.iter().position(|b| *b == b'\n')) {
            let end = scan + offset;
            let line = String::from_utf8_lossy(self.pending.get(consumed..end).unwrap_or_default());
            consumed = end + 1;
            scan = consumed;
            let Some(data) = line.trim_end().strip_prefix("data:") else { continue };
            let Ok(v) = serde_json::from_str::<Value>(data.trim_start()) else { continue };
            self.text.push_str(&self.delta(&v));
            // Previews stay small: the whole reply is read separately.
            if self.text.len() > MAX_RESPONSE {
                let cut = (0..=MAX_RESPONSE).rev().find(|i| self.text.is_char_boundary(*i)).unwrap_or(0);
                self.text.truncate(cut);
            }
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
        self.scanned = self.pending.len();
        // A line without its end yet waits; a body that isn't events doesn't grow without bound.
        if self.pending.len() > MAX_RESPONSE {
            self.pending.clear();
            self.scanned = 0;
        }
        self.text.len() > before
    }

    fn delta(&self, v: &Value) -> String {
        let s = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap_or("").to_string();
        match self.api {
            Api::Responses if v.get("type").and_then(Value::as_str) == Some("response.output_text.delta") => s("/delta"),
            Api::OpenAi => s("/choices/0/delta/content"),
            Api::Anthropic if s("/delta/type") == "text_delta" => s("/delta/text"),
            Api::Gemini => v
                .pointer("/candidates/0/content/parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect(),
            _ => String::new(),
        }
    }
}

fn http_error(resp: &HttpResponse) -> String {
    let text: String = String::from_utf8_lossy(&resp.body).chars().take(300).collect();
    if (200..300).contains(&resp.status) {
        format!("the provider sent something that isn't JSON: {text}")
    } else {
        format!("HTTP {}: {}", resp.status, text.trim())
    }
}

/// The message of an error body (the providers nest it under `error`; the ChatGPT plan's
/// admission check sometimes says `detail`).
fn error_message(v: &Value) -> Option<String> {
    let msg = match v.get("error") {
        Some(e) => e.get("message").and_then(Value::as_str).or_else(|| e.as_str())?,
        None => v.get("detail").and_then(|d| d.as_str().or_else(|| d.get("message").and_then(Value::as_str)))?,
    };
    Some(msg.chars().take(500).collect())
}

fn json_body(v: &Value) -> Option<String> {
    Some(v.to_string())
}

fn header(k: &str, v: &str) -> (String, String) {
    (k.to_string(), v.to_string())
}

// ---------------------------------------------------------------------------------------------
// OpenAI Chat Completions

fn openai_request(cfg: &Config, system: &str, turns: &[Turn], tools: &[ToolDef]) -> HttpRequest {
    let mut messages = vec![json!({"role": "system", "content": system})];
    for t in turns {
        match t {
            Turn::User(text) => messages.push(json!({"role": "user", "content": text})),
            Turn::Assistant { native: Some((Api::OpenAi, m)), .. } => messages.push(m.clone()),
            Turn::Assistant { text, calls, .. } => {
                let mut m = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
                if !calls.is_empty() {
                    m["tool_calls"] = calls
                        .iter()
                        .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.args.to_string()}}))
                        .collect();
                }
                messages.push(m);
            }
            Turn::Results(rs) => {
                for r in rs {
                    messages.push(json!({"role": "tool", "tool_call_id": r.id, "content": r.content}));
                }
            }
        }
    }
    let tools: Vec<Value> = tools
        .iter()
        .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
        .collect();
    let mut body = json!({"model": cfg.model.trim(), "messages": messages, "stream": true});
    // Some servers refuse an empty tool list.
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        // Left unset, the model's default effort; some models take tools only without reasoning.
        if cfg.reasoning == Some(false) {
            body["reasoning_effort"] = json!("none");
        }
    }
    let mut headers = vec![header("Content-Type", "application/json"), header("Accept", "text/event-stream")];
    if !cfg.key.trim().is_empty() {
        headers.push(header("Authorization", &format!("Bearer {}", cfg.key.trim())));
    }
    HttpRequest { method: "POST", url: format!("{}/chat/completions", cfg.base()), headers, body: json_body(&body) }
}

/// Whether OpenAI refused a Chat Completions request for reasoning alongside function tools (some
/// models take tools there only with `reasoning_effort` set to `none`). Requests leave
/// `reasoning_effort` unset, so a 400 about it can only be about the model's default.
pub fn refuses_reasoning_with_tools(e: &Error) -> bool {
    e.status == Some(400) && (e.param.as_deref() == Some("reasoning_effort") || e.detail.to_lowercase().contains("reasoning_effort"))
}

fn openai_reply(v: &Value) -> Result<Reply, String> {
    let choice = v.get("choices").and_then(|c| c.get(0)).ok_or("the reply has no choices")?;
    let msg = choice.get("message").ok_or("the reply has no message")?;
    let text = msg.get("content").and_then(Value::as_str).unwrap_or("").to_string();
    let calls = msg
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .enumerate()
                .map(|(i, c)| {
                    let f = c.get("function");
                    let args = f.and_then(|f| f.get("arguments"));
                    ToolCall {
                        id: c.get("id").and_then(Value::as_str).map_or_else(|| format!("call_{i}"), str::to_string),
                        name: f.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("").to_string(),
                        // Arguments come as JSON text; some servers send an object.
                        args: match args {
                            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({"_unparsed": s})),
                            Some(other) => other.clone(),
                            None => json!({}),
                        },
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let cut = match choice.get("finish_reason").and_then(Value::as_str) {
        Some("length") => Some(Note::new("The reply reached its length limit.")),
        Some("content_filter") => Some(Note::new("The provider's content filter stopped the reply.")),
        _ => None,
    };
    // Replayed as sent, minus fields some servers refuse in requests.
    let mut native = msg.clone();
    if let Some(o) = native.as_object_mut() {
        o.retain(|k, _| matches!(k.as_str(), "role" | "content" | "tool_calls" | "reasoning_content"));
    }
    Ok(Reply { text, calls, native, cut, failure: None })
}

// ---------------------------------------------------------------------------------------------
// OpenAI Responses (a ChatGPT plan's sign-in)

/// The namespace the assistant's tools are grouped in (a ChatGPT plan takes namespaced tools).
pub const TOOL_NAMESPACE: &str = "rustgis";

fn responses_headers(cfg: &Config) -> Vec<(String, String)> {
    vec![header("Authorization", &format!("Bearer {}", cfg.key.trim()))]
}

fn responses_request(cfg: &Config, system: &str, turns: &[Turn], tools: &[ToolDef]) -> HttpRequest {
    let mut input = Vec::new();
    for t in turns {
        match t {
            Turn::User(text) => input.push(json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]})),
            Turn::Assistant { native: Some((Api::Responses, items)), .. } => {
                input.extend(items.as_array().cloned().unwrap_or_default());
            }
            Turn::Assistant { text, calls, .. } => {
                if !text.trim().is_empty() {
                    input.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}));
                }
                for c in calls {
                    input.push(json!({"type": "function_call", "call_id": c.id, "name": c.name, "arguments": c.args.to_string()}));
                }
            }
            Turn::Results(rs) => {
                for r in rs {
                    input.push(json!({"type": "function_call_output", "call_id": r.id, "output": r.content}));
                }
            }
        }
    }
    let functions: Vec<Value> = tools
        .iter()
        .map(|t| json!({"type": "function", "name": t.name, "description": t.description, "parameters": t.schema, "strict": false}))
        .collect();
    let mut body = json!({
        "model": cfg.model.trim(),
        "instructions": system,
        "input": input,
        // A plan's requests stream and keep nothing server-side: the reasoning comes back
        // encrypted and is replayed with the conversation (`native`). The length of the reply
        // is the plan's to limit (`max_output_tokens` isn't accepted).
        "stream": true,
        "store": false,
    });
    // Older catalogs omit the capability; keep the existing behavior in that case.
    if cfg.reasoning != Some(false) {
        body["reasoning"] = json!({"effort": "medium"});
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    if !functions.is_empty() {
        body["tools"] = json!([{
            "type": "namespace",
            "name": TOOL_NAMESPACE,
            "description": "RustGIS: read and change the map, run commands and geoprocessing tools.",
            "tools": functions,
        }]);
        body["tool_choice"] = json!("auto");
        body["parallel_tool_calls"] = json!(true);
    }
    let mut headers = vec![header("Content-Type", "application/json"), header("Accept", "text/event-stream")];
    headers.extend(responses_headers(cfg));
    HttpRequest { method: "POST", url: format!("{}/responses", cfg.base()), headers, body: json_body(&body) }
}

/// The `data:` payloads of a server-sent event stream, parsed.
fn sse_events(body: &str) -> Vec<Value> {
    let mut events = Vec::new();
    let normalized = body.replace("\r\n", "\n");
    for block in normalized.split("\n\n") {
        let data: Vec<&str> = block.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim_start).collect();
        if data.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&data.join("\n")) {
            events.push(v);
        }
    }
    events
}

/// Whether an output item can be replayed once its turn's calls are dropped: calls have no
/// outputs to pair with, and reasoning items are refused without the item that followed them.
fn replayable_without_calls(item: &Value) -> bool {
    !matches!(item.get("type").and_then(Value::as_str), Some("function_call" | "reasoning"))
}

/// Reads a streamed Responses reply, read to its end: the reply counts only once the stream says
/// `response.completed`. Incomplete replies retain text but cannot run tools. The output items are those
/// of the final `response`, or those the stream finished (`response.output_item.done`). A plain
/// JSON reply (a server that didn't stream) is read the same way.
fn responses_reply(resp: &HttpResponse) -> Result<Reply, Error> {
    let text = String::from_utf8_lossy(&resp.body);
    if !(200..300).contains(&resp.status) {
        let v = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        return Err(Error::response(&v, resp));
    }
    let mut indexed_items: Vec<(u64, Value)> = Vec::new();
    let mut last: Option<Value> = None;
    let mut error: Option<Error> = None;
    let mut completed = false;
    let events = if text.trim_start().starts_with('{') {
        serde_json::from_str::<Value>(&text).ok().map(|v| vec![json!({"type": if v.get("status").and_then(Value::as_str) == Some("completed") { "response.completed" } else { "response.incomplete" }, "response": v})]).unwrap_or_default()
    } else {
        sse_events(&text)
    };
    for e in &events {
        match e.get("type").and_then(Value::as_str) {
            Some("response.output_item.done") => {
                if let Some(item) = e.get("item") {
                    let index = e.get("output_index").and_then(Value::as_u64).unwrap_or(indexed_items.len() as u64);
                    indexed_items.push((index, item.clone()));
                }
            }
            Some(kind @ ("response.completed" | "response.incomplete")) => {
                completed =
                    kind == "response.completed" && e.pointer("/response/status").and_then(Value::as_str).is_none_or(|s| s == "completed");
                last = e.get("response").cloned();
            }
            Some("response.failed") => {
                error = Some(Error::response(e, resp));
            }
            Some("error") => error = Some(Error::response(e, resp)),
            _ => {}
        }
    }
    if let Some(e) = error {
        return Err(e);
    }
    let last = last
        .ok_or_else(|| if events.is_empty() { http_error(resp) } else { "the stream ended before the reply was complete".to_string() })?;
    indexed_items.sort_by_key(|(index, _)| *index);
    let mut items: Vec<Value> = indexed_items.into_iter().map(|(_, item)| item).collect();
    if let Some(output) = last.get("output").and_then(Value::as_array).filter(|output| !output.is_empty()) {
        // The terminal response is authoritative and already ordered.
        items = output.clone();
    }
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut refused = None;
    for (i, item) in items.iter_mut().enumerate() {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for c in item.get("content").and_then(Value::as_array).into_iter().flatten() {
                    match c.get("type").and_then(Value::as_str) {
                        Some("output_text") => text.push_str(c.get("text").and_then(Value::as_str).unwrap_or("")),
                        Some("refusal") => refused = Some(c.get("refusal").and_then(Value::as_str).unwrap_or("").to_string()),
                        _ => {}
                    }
                }
            }
            Some("function_call") => {
                // A call without an id gets one, kept on the replayed item so its output pairs.
                let id = match item.get("call_id").and_then(Value::as_str) {
                    Some(id) => id.to_string(),
                    None => {
                        let id = format!("call_{i}");
                        if let Some(o) = item.as_object_mut() {
                            o.insert("call_id".into(), json!(id));
                        }
                        id
                    }
                };
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                calls.push(ToolCall {
                    id,
                    // Some models name a namespaced tool with its namespace.
                    name: name.strip_prefix(TOOL_NAMESPACE).and_then(|n| n.strip_prefix('.')).unwrap_or(name).to_string(),
                    args: match item.get("arguments") {
                        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_else(|_| json!({"_unparsed": s})),
                        Some(other) => other.clone(),
                        None => json!({}),
                    },
                });
            }
            _ => {}
        }
    }
    let mut cut = match last.pointer("/incomplete_details/reason").and_then(Value::as_str) {
        Some("max_output_tokens") => Some(Note::new("The reply reached its length limit.")),
        Some("content_filter") => Some(Note::new("The provider's content filter stopped the reply.")),
        Some(r) => Some(Note::with("The reply stopped early ({reason}).", [("reason", r.to_string())])),
        None => None,
    };
    if let Some(r) = refused {
        cut = Some(declined(r));
        calls.clear();
        items.retain(replayable_without_calls);
    }
    if !completed {
        calls.clear();
        items.retain(replayable_without_calls);
        if cut.is_none() {
            cut = Some(Note::new("The reply stopped before completion; no actions were run."));
        }
    }
    // Replayed as sent, minus the server's item ids (the conversation isn't stored there).
    let native: Vec<Value> = items
        .into_iter()
        .map(|mut item| {
            if let Some(o) = item.as_object_mut() {
                o.remove("id");
            }
            item
        })
        .collect();
    let failure = (!completed && last.get("error").is_some_and(Value::is_object)).then(|| Error::response(&last, resp));
    Ok(Reply { text, calls, native: Value::Array(native), cut, failure })
}

// ---------------------------------------------------------------------------------------------
// Anthropic Messages

/// Models that take the server-side refusal fallback (Claude API only).
fn takes_fallback(model: &str) -> bool {
    ["claude-opus-5", "claude-fable-5-1", "claude-sonnet-5-5"].iter().any(|m| model.starts_with(m))
}

fn anthropic_request(cfg: &Config, system: &str, turns: &[Turn], tools: &[ToolDef]) -> HttpRequest {
    let mut messages: Vec<Value> = Vec::new();
    // Consecutive turns of one role are merged into one message.
    let mut push = |role: &str, blocks: Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        if let Some(last) = messages.last_mut()
            && last["role"] == role
            && let Some(c) = last.get_mut("content").and_then(Value::as_array_mut)
        {
            c.extend(blocks);
            return;
        }
        messages.push(json!({"role": role, "content": blocks}));
    };
    for t in turns {
        match t {
            Turn::User(text) => push("user", vec![json!({"type": "text", "text": text})]),
            Turn::Assistant { native: Some((Api::Anthropic, content)), .. } => {
                push("assistant", content.as_array().cloned().unwrap_or_default());
            }
            Turn::Assistant { text, calls, .. } => {
                let mut blocks = Vec::new();
                if !text.trim().is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for c in calls {
                    blocks.push(json!({"type": "tool_use", "id": c.id, "name": c.name, "input": object(&c.args)}));
                }
                push("assistant", blocks);
            }
            Turn::Results(rs) => push(
                "user",
                rs.iter().map(|r| json!({"type": "tool_result", "tool_use_id": r.id, "content": r.content, "is_error": r.error})).collect(),
            ),
        }
    }
    let tools: Vec<Value> = tools.iter().map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema})).collect();
    let model = cfg.model.trim();
    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "messages": messages,
        // Caches the system prompt, tools and history between turns.
        "cache_control": {"type": "ephemeral"},
        "stream": true,
    });
    let mut headers = vec![
        header("Content-Type", "application/json"),
        header("Accept", "text/event-stream"),
        header("x-api-key", cfg.key.trim()),
        header("anthropic-version", "2023-06-01"),
        // Lets the browser app call the API directly (with the user's own key).
        header("anthropic-dangerous-direct-browser-access", "true"),
    ];
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    // A request a model declines is retried on another (Claude API only).
    if cfg.base() == "https://api.anthropic.com" && takes_fallback(model) {
        body["fallbacks"] = json!("default");
        headers.push(header("anthropic-beta", "server-side-fallback-2026-07-01"));
    }
    HttpRequest { method: "POST", url: format!("{}/v1/messages", cfg.base()), headers, body: json_body(&body) }
}

fn anthropic_reply(v: &Value) -> Result<Reply, String> {
    let content = v.get("content").and_then(Value::as_array).ok_or("the reply has no content")?;
    let mut text = String::new();
    let mut calls = Vec::new();
    for b in content {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(b.get("text").and_then(Value::as_str).unwrap_or("")),
            Some("tool_use") => calls.push(ToolCall {
                id: b.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: b.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                args: b.get("input").cloned().unwrap_or_else(|| json!({})),
            }),
            _ => {}
        }
    }
    let cut = match v.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => Some(Note::new("The reply reached its length limit.")),
        Some("refusal") => Some(
            v.pointer("/stop_details/explanation")
                .and_then(Value::as_str)
                .map_or_else(|| declined(String::new()), |e| declined(e.to_string())),
        ),
        _ => None,
    };
    // A refused reply's partial calls must not run.
    if v.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        calls.clear();
    }
    Ok(Reply { text, calls, native: Value::Array(content.clone()), cut, failure: None })
}

// ---------------------------------------------------------------------------------------------
// Gemini

/// Gemini takes a subset of JSON Schema: an object needs properties, so a free-form object
/// parameter is sent as JSON text instead (the tools accept either).
fn gemini_schema(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let is_object = o.get("type").and_then(Value::as_str) == Some("object");
            let has_props = o.get("properties").and_then(Value::as_object).is_some_and(|p| !p.is_empty());
            if is_object && !has_props {
                let desc = o.get("description").and_then(Value::as_str).unwrap_or("A JSON object");
                return json!({"type": "string", "description": format!("{desc} (as JSON text)")});
            }
            let mut out = Map::new();
            for (k, v) in o {
                if k == "additionalProperties" || k == "$schema" {
                    continue;
                }
                let v = if k == "properties" {
                    Value::Object(v.as_object().map(|p| p.iter().map(|(k, v)| (k.clone(), gemini_schema(v))).collect()).unwrap_or_default())
                } else if k == "items" {
                    gemini_schema(v)
                } else {
                    v.clone()
                };
                out.insert(k.clone(), v);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// The prefix of call ids made up for Gemini calls that came without one.
const MADE_UP: &str = "rustgis-call-";

fn gemini_request(cfg: &Config, system: &str, turns: &[Turn], tools: &[ToolDef]) -> HttpRequest {
    let mut contents = Vec::new();
    for t in turns {
        match t {
            Turn::User(text) => contents.push(json!({"role": "user", "parts": [{"text": text}]})),
            Turn::Assistant { native: Some((Api::Gemini, c)), .. } => contents.push(c.clone()),
            Turn::Assistant { text, calls, .. } => {
                let mut parts = Vec::new();
                if !text.trim().is_empty() {
                    parts.push(json!({"text": text}));
                }
                for c in calls {
                    parts.push(json!({"functionCall": {"name": c.name, "args": object(&c.args)}}));
                }
                if !parts.is_empty() {
                    contents.push(json!({"role": "model", "parts": parts}));
                }
            }
            Turn::Results(rs) => {
                let parts: Vec<Value> = rs
                    .iter()
                    .map(|r| {
                        let response = if r.error { json!({"error": r.content}) } else { json!({"result": r.content}) };
                        let mut f = json!({"name": r.name, "response": response});
                        // Ids Gemini gave (not the ones made up for it) go back with the result.
                        if !r.id.starts_with(MADE_UP) {
                            f["id"] = json!(r.id);
                        }
                        json!({"functionResponse": f})
                    })
                    .collect();
                contents.push(json!({"role": "user", "parts": parts}));
            }
        }
    }
    let decls: Vec<Value> =
        tools.iter().map(|t| json!({"name": t.name, "description": t.description, "parameters": gemini_schema(&t.schema)})).collect();
    let mut body = json!({
        "systemInstruction": {"parts": [{"text": system}]},
        "contents": contents,
    });
    if !decls.is_empty() {
        body["tools"] = json!([{"functionDeclarations": decls}]);
    }
    let model = cfg.model.trim().trim_start_matches("models/");
    HttpRequest {
        method: "POST",
        url: format!("{}/models/{}:streamGenerateContent?alt=sse", cfg.base(), model),
        headers: vec![
            header("Content-Type", "application/json"),
            header("Accept", "text/event-stream"),
            header("x-goog-api-key", cfg.key.trim()),
        ],
        body: json_body(&body),
    }
}

fn gemini_reply(v: &Value) -> Result<Reply, String> {
    let Some(cand) = v.get("candidates").and_then(|c| c.get(0)) else {
        let why = v.pointer("/promptFeedback/blockReason").and_then(Value::as_str).unwrap_or("no reason given");
        return Err(format!("Gemini sent no answer ({why})"));
    };
    let content = cand.get("content").cloned().unwrap_or_else(|| json!({"role": "model", "parts": []}));
    let mut text = String::new();
    let mut calls = Vec::new();
    for (i, p) in content.get("parts").and_then(Value::as_array).into_iter().flatten().enumerate() {
        if p.get("thought").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(t) = p.get("text").and_then(Value::as_str) {
            text.push_str(t);
        }
        if let Some(f) = p.get("functionCall") {
            calls.push(ToolCall {
                id: f.get("id").and_then(Value::as_str).map_or_else(|| format!("{MADE_UP}{i}"), str::to_string),
                name: f.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                args: f.get("args").cloned().unwrap_or_else(|| json!({})),
            });
        }
    }
    let cut = match cand.get("finishReason").and_then(Value::as_str) {
        Some("MAX_TOKENS") => Some(Note::new("The reply reached its length limit.")),
        Some(r @ ("SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII")) => {
            Some(Note::with("Gemini stopped the reply ({reason}).", [("reason", r.to_string())]))
        }
        _ => None,
    };
    let mut native = content;
    if native.get("role").is_none() {
        native["role"] = json!("model");
    }
    Ok(Reply { text, calls, native, cut, failure: None })
}

/// `v` if it is an object, else an empty one (providers require objects for tool input).
fn object(v: &Value) -> Value {
    if v.is_object() { v.clone() } else { json!({}) }
}

// ---------------------------------------------------------------------------------------------
// Model lists

/// The request listing the provider's models.
pub fn models_request(cfg: &Config) -> HttpRequest {
    let key = cfg.key.trim();
    match cfg.api() {
        Api::OpenAi => {
            let mut headers = Vec::new();
            if !key.is_empty() {
                headers.push(header("Authorization", &format!("Bearer {key}")));
            }
            HttpRequest { method: "GET", url: format!("{}/models", cfg.base()), headers, body: None }
        }
        Api::Responses => HttpRequest { method: "GET", url: format!("{}/models", cfg.base()), headers: responses_headers(cfg), body: None },
        Api::Anthropic => HttpRequest {
            method: "GET",
            url: format!("{}/v1/models?limit=1000", cfg.base()),
            headers: vec![
                header("x-api-key", key),
                header("anthropic-version", "2023-06-01"),
                header("anthropic-dangerous-direct-browser-access", "true"),
            ],
            body: None,
        },
        Api::Gemini => HttpRequest {
            method: "GET",
            url: format!("{}/models?pageSize=1000", cfg.base()),
            headers: vec![header("x-goog-api-key", key)],
            body: None,
        },
    }
}

/// The model ids in a model list, sorted.
pub fn parse_models(api: Api, resp: &HttpResponse) -> Result<Vec<String>, String> {
    let v: Value = serde_json::from_slice(&resp.body).map_err(|_| http_error(resp))?;
    if !(200..300).contains(&resp.status) {
        return Err(error_message(&v).unwrap_or_else(|| http_error(resp)));
    }
    let mut ids: Vec<String> = match api {
        Api::OpenAi | Api::Anthropic => v
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|m| m.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect(),
        // A ChatGPT plan's catalog lists its models with a slug and whether they are shown.
        Api::Responses => v
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|m| m.get("visibility").and_then(Value::as_str) == Some("list"))
            .filter_map(|m| m.get("slug").and_then(Value::as_str))
            .map(str::to_string)
            .collect(),
        Api::Gemini => v
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|m| {
                m.get("supportedGenerationMethods")
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.iter().any(|x| x.as_str() == Some("generateContent")))
            })
            .filter_map(|m| m.get("name").and_then(Value::as_str))
            .map(|n| n.trim_start_matches("models/").to_string())
            .collect(),
    };
    // An id with control characters could not be sent back, and would garble a terminal.
    ids.retain(|id| !id.chars().any(char::is_control));
    // The ChatGPT catalog comes in its own order (the first is the one to suggest).
    if api != Api::Responses {
        ids.sort();
        ids.dedup();
    }
    ids.truncate(2000);
    Ok(ids)
}

/// A model offered by the selected ChatGPT registration.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    /// The identifier sent to inference.
    pub id: String,
    /// The catalog's display name.
    pub name: String,
    /// Reasoning support, or unknown for older catalogs.
    pub reasoning: Option<bool>,
}

/// Reads the visible ChatGPT catalog, in server order.
pub fn parse_catalog(resp: &HttpResponse) -> Result<Vec<Model>, Error> {
    let v: Value = serde_json::from_slice(&resp.body).map_err(|_| Error::response(&Value::Null, resp))?;
    if !(200..300).contains(&resp.status) {
        return Err(Error::response(&v, resp));
    }
    Ok(v.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| m.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|m| {
            let id = m.get("slug")?.as_str()?;
            let reasoning = m
                .get("supports_reasoning")
                .and_then(Value::as_bool)
                .or_else(|| m.get("supported_reasoning_levels").and_then(Value::as_array).map(|a| !a.is_empty()));
            Some(Model { id: id.into(), name: m.get("display_name").and_then(Value::as_str).unwrap_or(id).into(), reasoning })
        })
        .take(2000)
        .collect())
}

/// Uses the first offered model when the current one is absent, and applies its capabilities.
pub fn use_catalog(cfg: &mut Config, models: &[Model], explicit: bool) {
    let selected = models.iter().find(|m| m.id == cfg.model).or_else(|| if explicit { None } else { models.first() });
    cfg.reasoning = selected.and_then(|m| m.reasoning);
    if let Some(model) = selected {
        cfg.model = model.id.clone();
    }
}
