//! Sending requests on the desktop and the command line (blocking: call it off the UI thread).

use crate::provider::{HttpRequest, HttpResponse, allowed_url, reply_limit};
use crate::{Error, error::TransportFailure};
use rustgis_engine::data::diagnostics;

/// Sends `req` and reads the response, whatever its status.
pub fn send(req: &HttpRequest) -> Result<HttpResponse, Error> {
    send_timeout(req, std::time::Duration::from_secs(600))
}

/// Sends `req`, handing each piece of the response body to `on_chunk` as it arrives (for a
/// streamed reply's live text), and reads the whole response.
pub fn send_streaming(req: &HttpRequest, on_chunk: &mut dyn FnMut(&[u8])) -> Result<HttpResponse, Error> {
    send_with(req, std::time::Duration::from_secs(600), Some(on_chunk))
}

/// Sends with an overall deadline, including reading the response body.
pub fn send_timeout(req: &HttpRequest, timeout: std::time::Duration) -> Result<HttpResponse, Error> {
    send_with(req, timeout, None)
}

fn send_with(req: &HttpRequest, timeout: std::time::Duration, mut on_chunk: Option<&mut dyn FnMut(&[u8])>) -> Result<HttpResponse, Error> {
    // The address checked is the one used.
    let url = req.url.trim();
    if !allowed_url(url) {
        return Err(
            diagnostics::message_with_address(url, "only HTTPS addresses, or plain HTTP on this computer or network, are allowed").into()
        );
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        // Models can think for minutes on a hard request.
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        // An API key is never sent on to another address.
        .max_redirects(0)
        .tls_config(rustgis_engine::data::http::tls_config())
        .build()
        .into();
    let ua = concat!("RustGIS/", env!("CARGO_PKG_VERSION"));
    let result = match (req.method, &req.body) {
        ("POST", body) => {
            let mut b = agent.post(url).header("User-Agent", ua);
            for (k, v) in &req.headers {
                b = b.header(k.as_str(), v.as_str());
            }
            b.send(body.as_deref().unwrap_or(""))
        }
        _ => {
            let mut b = agent.get(url).header("User-Agent", ua);
            for (k, v) in &req.headers {
                b = b.header(k.as_str(), v.as_str());
            }
            b.call()
        }
    };
    let mut resp = result.map_err(|e| transport_error(&e, url))?;
    let status = resp.status().as_u16();
    let headers = crate::provider::KEPT_HEADERS
        .into_iter()
        .filter_map(|name| Some((name.to_string(), resp.headers().get(name)?.to_str().ok()?.to_string())))
        .collect();
    let limit = reply_limit(req);
    let mut reader = resp.body_mut().with_config().limit(limit as u64).reader();
    let mut body = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = std::io::Read::read(&mut reader, &mut chunk)
            .map_err(|e| Error::transport(TransportFailure::Interrupted, format!("reading the reply: {e}")))?;
        let Some(piece) = chunk.get(..n).filter(|p| !p.is_empty()) else { break };
        if (200..300).contains(&status)
            && let Some(f) = on_chunk.as_mut()
        {
            f(piece);
        }
        body.extend_from_slice(piece);
    }
    Ok(HttpResponse { status, body, headers })
}

/// The scheme and authority of an already-sanitized address, without its omission suffix. A
/// root address keeps the `/` that separates it from a following `: `; a path is dropped whole.
fn host(shown: &str) -> &str {
    let url = shown.strip_suffix("[query omitted]").or_else(|| shown.strip_suffix("[fragment omitted]")).unwrap_or(shown);
    let Some((scheme, rest)) = url.split_once("://") else { return url };
    let end = match rest.find('/') {
        Some(at) if at + 1 == rest.len() => url.len(),
        Some(at) => scheme.len() + 3 + at,
        None => url.len(),
    };
    url.get(..end).unwrap_or(url)
}

/// Only connection failures can be replayed. Timeouts may already have consumed a long request.
fn transport_error(e: &ureq::Error, url: &str) -> Error {
    use std::io::ErrorKind;
    let kind = match e {
        ureq::Error::Timeout(_) => TransportFailure::Timeout,
        ureq::Error::Io(e) if e.kind() == ErrorKind::TimedOut => TransportFailure::Timeout,
        ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => TransportFailure::Connection,
        ureq::Error::Io(e) if matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::AddrNotAvailable | ErrorKind::NotConnected) => {
            TransportFailure::Connection
        }
        _ => TransportFailure::Interrupted,
    };
    // Establish known-address boundaries before applying the stricter host-only display policy.
    // Replacing raw text first could match a prefix of another address's longer credential.
    let message = diagnostics::message_with_address(url, &e.to_string());
    let shown = diagnostics::address_in_message(url);
    let message = if shown.starts_with("https://") || shown.starts_with("http://") {
        let host = host(&shown);
        if host == shown { message } else { message.replace(&shown, host) }
    } else {
        message
    };
    Error::transport(kind, format!("couldn't reach {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::Duration,
    };

    #[test]
    fn rejected_secret_addresses_keep_the_policy_reason() {
        let req = HttpRequest { method: "GET", url: "ftp://models.test/private?token=secret value".into(), headers: vec![], body: None };
        let error = send(&req).unwrap_err().to_string();
        let error = diagnostics::redact(&diagnostics::redact(&error));
        assert!(error.contains("only HTTPS addresses"), "{error}");
        assert!(!error.contains("secret") && !error.contains("value"), "{error}");
    }

    #[test]
    fn transport_errors_keep_reasons_without_revealing_request_paths() {
        let url = "https://user:password@models.test/private-key?token=secret value";
        let cause = ureq::Error::Io(std::io::Error::other(format!("request {url} failed; retry {url}: connection closed")));
        let error = transport_error(&cause, url);
        assert_eq!(error.transport, Some(TransportFailure::Interrupted));
        let error = diagnostics::redact(&diagnostics::redact(&error.to_string()));
        assert!(error.contains("couldn't reach https://models.test: ") && error.contains("connection closed"), "{error}");
        for hidden in ["user", "password", "private-key", "secret", "value"] {
            assert!(!error.contains(hidden), "{error}");
        }
    }

    #[test]
    fn omitted_userinfo_is_not_restored_by_host_projection() {
        for url in ["https://alice:hidden/x@models.test/private", "https://alice/x:hidden@models.test/private"] {
            let cause = ureq::Error::Io(std::io::Error::other(format!("request {url}: connection closed")));
            let error = transport_error(&cause, url).to_string();
            for secret in ["alice", "hidden", "private"] {
                assert!(!error.contains(secret), "{url} -> {error}");
            }
            assert!(error.contains("connection closed"), "{error}");
            assert_eq!(diagnostics::redact(&diagnostics::redact(&error)), error);
        }
    }

    #[test]
    fn host_only_addresses_keep_the_host_after_repeated_redaction() {
        let url = "https://models.test?token=hidden value";
        let cause = ureq::Error::Io(std::io::Error::other("connection closed"));
        let error = transport_error(&cause, url).to_string();
        let error = diagnostics::redact(&diagnostics::redact(&error));
        assert!(error.contains("https://models.test") && error.contains("connection closed"), "{error}");
        assert!(!error.contains("hidden") && !error.contains("value"), "{error}");
    }

    #[test]
    fn unknown_transport_addresses_keep_conservative_credential_boundaries() {
        let url = "https://models.test/private-key?token=abc";
        for reason in [
            format!("request {url}XYZ credential tail: connection closed"),
            "request https://other.test/a?token=unknown credential tail: connection closed".into(),
        ] {
            let cause = ureq::Error::Io(std::io::Error::other(reason));
            let error = transport_error(&cause, url).to_string();
            let error = diagnostics::redact(&diagnostics::redact(&error));
            assert!(error.contains("couldn't reach https://models.test"), "{error}");
            for hidden in ["abc", "XYZ", "unknown", "credential", "tail", "connection closed"] {
                assert!(!error.contains(hidden), "{error}");
            }
        }
    }

    #[test]
    fn native_transport_preserves_connection_timeout_and_interruption_types() {
        // Check the typed refusal policy independently of OS-specific TCP retry timing.
        let refused = ureq::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        let e = transport_error(&refused, "http://127.0.0.1").auth_context();
        assert_eq!(e.transport, Some(TransportFailure::Connection));
        assert!(crate::provider::retry_delay(&Err(e), 0).is_some());
        // Windows may take several seconds to report a refused loopback connection.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let req = HttpRequest { method: "GET", url: format!("http://{addr}"), headers: vec![], body: None };
        let e = send_timeout(&req, Duration::from_secs(10)).unwrap_err().auth_context();
        assert_eq!(e.transport, Some(TransportFailure::Connection));
        assert!(crate::provider::retry_delay(&Err(e), 0).is_some());
        for timeout in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let (release, released) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0; 4096];
                let _ = stream.read(&mut buf);
                if timeout {
                    // Stay silent until the client has observed its deadline; no scheduler race.
                    let _ = released.recv();
                } else {
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort").unwrap();
                }
            });
            let req = HttpRequest { method: "GET", url: format!("http://{addr}"), headers: vec![], body: None };
            let deadline = if timeout { Duration::from_millis(50) } else { Duration::from_secs(5) };
            let e = send_timeout(&req, deadline).unwrap_err();
            assert_eq!(e.transport, Some(if timeout { TransportFailure::Timeout } else { TransportFailure::Interrupted }));
            assert!(crate::provider::retry_delay(&Err(e), 0).is_none());
            let _ = release.send(());
            server.join().unwrap();
        }
    }

    #[test]
    fn native_transport_keeps_only_retry_and_request_id_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 4096];
            let _ = stream.read(&mut buf);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nRetry-After: 2\r\nX-Request-Id: request-1\r\nSet-Cookie: secret-cookie\r\nX-Unused: secret-extra\r\n\r\n{}").unwrap();
        });
        let req = HttpRequest { method: "GET", url: format!("http://{addr}"), headers: vec![], body: None };
        let response = send_timeout(&req, Duration::from_secs(5)).unwrap();
        assert_eq!(response.header("retry-after"), Some("2"));
        assert_eq!(response.request_id(), Some("request-1"));
        assert_eq!(response.headers.len(), 2);
        assert!(response.header("set-cookie").is_none() && response.header("x-unused").is_none());
        server.join().unwrap();
    }

    #[test]
    fn rejected_http_responses_never_feed_streaming_preview() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 4096];
            let _ = stream.read(&mut buf);
            let body = "data: unauthorized\n\n";
            let reply = format!("HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            let _ = stream.write_all(reply.as_bytes());
        });
        let req = HttpRequest { method: "GET", url: format!("http://{addr}"), headers: vec![], body: None };
        let mut preview = Vec::new();
        let response = send_with(&req, Duration::from_secs(5), Some(&mut |piece| preview.extend_from_slice(piece))).unwrap();
        assert_eq!(response.status, 401);
        assert!(!response.body.is_empty());
        assert!(preview.is_empty());
        server.join().unwrap();
    }
}
