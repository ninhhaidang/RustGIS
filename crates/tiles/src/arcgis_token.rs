//! ArcGIS Server connection tokens: the credential a saved connection logs in with, and the
//! `generateToken` request that makes one from a user name and password.
//!
//! A token is sent as the request's one `token` query parameter ([`crate::logins::Method::Arcgis`]),
//! which works from a web page without a CORS preflight. Only the token and its expiry are ever
//! kept (the private payload below); the password lives in one request's form and nowhere else.
//!
//! Token generation is its own transport, separate from the portal's OAuth exchange
//! ([`crate::arcgis_auth`]): HTTPS only, at the connection's own origin, no redirects, no cookies,
//! no other login, 30 seconds, at most 1 MiB of answer. A failure is a fixed sentence: neither the
//! server's description nor a transport's error text (which may quote a request) is passed on.
//! The sans-I/O parts (the form, the answer, the payload) are here; the transport is native
//! (`ureq`) or installed by the browser ([`set_generator`]).

use std::sync::{Arc, LazyLock, Mutex};

use rustgis_data::secrets::Secret;
use serde_json::{Value, json};

use crate::cache::Still;

/// The longest token, in bytes.
pub const MAX_TOKEN: usize = crate::logins::MAX_VALUE;
/// The longest token endpoint, in bytes.
pub const MAX_ENDPOINT: usize = 2048;
/// How long a generation may take before it is given up, in seconds.
pub const DEADLINE_SECS: u64 = 30;
/// The most bytes of answer read.
pub const MAX_ANSWER: usize = 1 << 20;
/// The lifetime asked of a generated token, in minutes. The server's `expires` is what counts.
const EXPIRATION_MINUTES: &str = "60";

/// A token that can't be sent in a request.
pub const BAD_TOKEN: &str = "The ArcGIS token must be at most 4096 ASCII letters, digits or - _ . ~ + / = characters.";
/// An expiry that isn't a future time.
pub const BAD_EXPIRY: &str = "expiresAt must be a Unix time in milliseconds, in the future.";
/// A token already past its expiry.
pub const EXPIRED: &str = "The ArcGIS token has expired. Log in to this connection again.";
/// A token that expires, on a computer whose clock can't be read.
pub const NO_CLOCK: &str = "This computer's clock can't be read, so an expiring ArcGIS token can't be used.";
/// A token endpoint the connection can't use.
pub const BAD_ENDPOINT: &str =
    "The token endpoint must be an https:// address at this connection's own origin, without user information, a query or a fragment.";
/// The server answered, with no token.
pub const REFUSED: &str = "The server didn't give a token. Check the user name, password and token endpoint.";
/// An answer that isn't an ArcGIS token.
pub const BAD_ANSWER: &str = "The server's answer wasn't an ArcGIS token.";
/// The request failed before an answer.
pub const FAILED: &str = "The token request failed.";
/// Nothing answered in time.
pub const TIMED_OUT: &str = "The server didn't answer the token request in time.";
/// An answer past its limit.
pub const TOO_LARGE: &str = "The server's answer to the token request was too large.";
/// A generation given up.
pub const CANCELLED: &str = "The token request was cancelled.";
/// A worker that couldn't be started.
pub const NO_WORKER: &str = "The token request couldn't be started.";
/// A build with no way to send the request.
pub const NO_TRANSPORT: &str = "This app can't request ArcGIS tokens.";

/// A nonempty ASCII token of at most [`MAX_TOKEN`] bytes, of the characters ArcGIS tokens use
/// (letters, digits and `-_.~+/=`), so it is the same text in an address, a form and a log, and
/// redaction can recognize it.
pub fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN && token.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.~+/=".contains(&b))
}

/// Unix milliseconds now, or `None` when the clock can't be read.
pub fn now_millis() -> Option<i64> {
    let since = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// Whether a token expiring at `expires_at` (none: it isn't known to) is past it at `now`. A
/// clock that can't be read refuses an expiring token rather than treating it as unexpired.
pub fn expired(expires_at: Option<i64>, now: Option<i64>) -> bool {
    expires_at.is_some_and(|at| now.is_none_or(|now| now >= at))
}

/// A token and when it stops working. Deliberately neither `Debug` nor `Clone`: nothing prints
/// or copies it by accident.
pub struct Credential {
    pub token: Secret,
    /// Unix milliseconds, when known.
    pub expires_at: Option<i64>,
}

impl Credential {
    /// A token as it can be sent, with its optional expiry (a positive time).
    pub fn new(token: &str, expires_at: Option<i64>) -> Result<Self, &'static str> {
        if !valid_token(token) {
            return Err(BAD_TOKEN);
        }
        if expires_at.is_some_and(|at| at <= 0) {
            return Err(BAD_EXPIRY);
        }
        Ok(Self { token: Secret::new(token), expires_at })
    }

    /// The private payload kept in a session or a saved login:
    /// `{"version":1,"token":"…","expiresAt":null|<Unix milliseconds>}`.
    pub fn payload(&self) -> Secret {
        Secret::new(json!({"version": 1, "token": self.token.expose(), "expiresAt": self.expires_at}).to_string())
    }

    /// Reads [`Credential::payload`]: exactly its three fields. What it can't read (another
    /// version, another shape) is refused without quoting it.
    pub fn parse(payload: &str) -> Result<Self, &'static str> {
        let Ok(Value::Object(o)) = serde_json::from_str::<Value>(payload) else { return Err(BAD_TOKEN) };
        if o.len() != 3 || o.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(BAD_TOKEN);
        }
        let token = o.get("token").and_then(Value::as_str).ok_or(BAD_TOKEN)?;
        let expires_at = match o.get("expiresAt").ok_or(BAD_TOKEN)? {
            Value::Null => None,
            value => Some(value.as_i64().ok_or(BAD_EXPIRY)?),
        };
        Self::new(token, expires_at)
    }

    /// Whether it is past its expiry at `now`.
    pub fn is_expired(&self, now: Option<i64>) -> bool {
        expired(self.expires_at, now)
    }
}

/// Checks a token endpoint against the connection's `origin`: an absolute https:// address on
/// exactly that origin, with no user information, query or fragment. It is never found in a
/// service's answer or guessed from a path.
pub fn check_endpoint(endpoint: &str, origin: &str) -> Result<(), &'static str> {
    if endpoint.len() > MAX_ENDPOINT
        || !crate::address::is_https(endpoint)
        || endpoint.contains(['?', '#'])
        || crate::logins::origin(endpoint).as_deref() != Some(origin)
    {
        return Err(BAD_ENDPOINT);
    }
    Ok(())
}

/// The `generateToken` form: `f=json`, the user, the password, `client=requestip` and a lifetime.
pub fn form(username: &str, password: &str) -> Secret {
    Secret::new(crate::arcgis_auth::form(&[
        ("f", "json"),
        ("username", username),
        ("password", password),
        ("client", "requestip"),
        ("expiration", EXPIRATION_MINUTES),
    ]))
}

/// What a `generateToken` answer gives: a valid token and an integer `expires` (Unix
/// milliseconds) after `now`. An expiry is never assumed, an OAuth `access_token` answer isn't
/// this protocol, and no server text is passed on.
pub fn from_answer(status: u16, body: &[u8], now: Option<i64>) -> Result<Credential, String> {
    if !(200..300).contains(&status) {
        return Err(format!("The server refused the token request (HTTP {status})."));
    }
    let Ok(Value::Object(o)) = serde_json::from_slice::<Value>(body) else { return Err(BAD_ANSWER.into()) };
    if o.contains_key("error") {
        return Err(REFUSED.into());
    }
    let (Some(token), Some(expires)) = (o.get("token").and_then(Value::as_str), o.get("expires").and_then(Value::as_i64)) else {
        return Err(BAD_ANSWER.into());
    };
    if expired(Some(expires), Some(now.ok_or(NO_CLOCK)?)) {
        return Err(BAD_ANSWER.into());
    }
    Credential::new(token, Some(expires)).map_err(|_| BAD_ANSWER.into())
}

/// How a generation's answer is delivered, once.
pub type Done = Box<dyn FnOnce(Result<Credential, String>) + Send>;

type Generator = dyn Fn(String, Secret, Still, Done) + Send + Sync;

static GENERATOR: LazyLock<Mutex<Option<Arc<Generator>>>> = LazyLock::new(Mutex::default);

/// Replaces how a token is requested (the browser's `fetch`): the endpoint, the form, the
/// request's predicate (false once it is cancelled or its authority revoked) and what to call
/// once, with a credential made by [`from_answer`]. The transport sends the form to the endpoint
/// only: no redirects, cookies, referrer or other login.
pub fn set_generator(f: impl Fn(String, Secret, Still, Done) + Send + Sync + 'static) {
    *GENERATOR.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
}

/// Requests a token, off the caller's thread, and calls `done` exactly once (even when nothing
/// could be started). `endpoint` has been checked by [`check_endpoint`]; this refuses anything
/// but https:// again. `still` turns false when the request is cancelled: the answer is then
/// discarded, and a transport stops as soon as it next looks.
pub fn generate(endpoint: String, body: Secret, still: Still, done: Done) {
    if !crate::address::is_https(&endpoint) {
        done(Err(BAD_ENDPOINT.into()));
        return;
    }
    let installed = GENERATOR.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(send) = installed {
        send(endpoint, body, still, done);
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    native::start(endpoint, body, still, done);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (body, still);
        done(Err(NO_TRANSPORT.into()));
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::io::Read;
    use std::sync::{Arc, LazyLock, Mutex};
    use std::time::Duration;

    use super::{
        BAD_ANSWER, CANCELLED, Credential, DEADLINE_SECS, Done, FAILED, MAX_ANSWER, NO_WORKER, Secret, TIMED_OUT, TOO_LARGE, from_answer,
        now_millis,
    };
    use crate::cache::{Still, check_still};

    /// One agent, made once (its TLS setup reads the system's certificates). It never follows a
    /// redirect, which would replay the password; keeps no connection; has no cookie jar.
    static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
        ureq::Agent::config_builder()
            .tls_config(rustgis_data::http::tls_config())
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(DEADLINE_SECS)))
            .user_agent(concat!("RustGIS/", env!("CARGO_PKG_VERSION"), " (+https://rustgis.app)"))
            .max_redirects(0)
            .max_idle_connections(0)
            .build()
            .into()
    });

    /// Calls `done` once, whichever of the worker and a failed start gets here first.
    pub(super) fn finish(slot: &Mutex<Option<Done>>, result: Result<Credential, String>) {
        let done = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(done) = done {
            done(result);
        }
    }

    pub(super) fn start(endpoint: String, body: Secret, still: Still, done: Done) {
        let slot = Arc::new(Mutex::new(Some(done)));
        let worker = slot.clone();
        let spawned = std::thread::Builder::new().name("arcgis-token".into()).spawn(move || {
            let result = run(&AGENT, &endpoint, &body, &still);
            finish(&worker, result);
        });
        if spawned.is_err() {
            finish(&slot, Err(NO_WORKER.into()));
        }
    }

    /// Sends the form and reads the answer. The cancellation predicate is looked at before the
    /// request is prepared, immediately before it is sent, and between chunks of the answer; one
    /// blocking network call may run on to the deadline, and its answer is then discarded.
    pub(super) fn run(agent: &ureq::Agent, endpoint: &str, body: &Secret, still: &Still) -> Result<Credential, String> {
        let still = Some(still.clone());
        let cancelled = |_: String| CANCELLED.to_string();
        check_still(&still).map_err(cancelled)?;
        let request = agent.post(endpoint).header("Content-Type", "application/x-www-form-urlencoded").header("Accept", "application/json");
        check_still(&still).map_err(cancelled)?;
        let mut response = request.send(body.expose()).map_err(|e| match e {
            ureq::Error::Timeout(_) => TIMED_OUT.to_string(),
            _ => FAILED.to_string(),
        })?;
        let status = response.status().as_u16();
        let mut reader = response.body_mut().with_config().limit(MAX_ANSWER as u64 + 1).reader();
        let mut answer = Vec::new();
        let mut chunk = [0u8; 16 << 10];
        loop {
            check_still(&still).map_err(cancelled)?;
            let n = reader.read(&mut chunk).map_err(|e| match e.kind() {
                std::io::ErrorKind::TimedOut => TIMED_OUT.to_string(),
                _ => FAILED.to_string(),
            })?;
            if n == 0 {
                break;
            }
            answer.extend_from_slice(chunk.get(..n).ok_or(BAD_ANSWER)?);
            if answer.len() > MAX_ANSWER {
                return Err(TOO_LARGE.into());
            }
        }
        check_still(&still).map_err(cancelled)?;
        from_answer(status, &answer, now_millis())
    }
}

#[cfg(test)]
mod tests;
