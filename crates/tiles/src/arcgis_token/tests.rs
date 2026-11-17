use super::*;

const FUTURE: i64 = 4_102_444_800_000; // 2100-01-01
const NOW: i64 = 1_800_000_000_000;
/// Long gone on any clock.
const PAST: i64 = 1_000_000_000_000;

fn answer(token: &str, expires: i64) -> Vec<u8> {
    json!({"token": token, "expires": expires, "ssl": true}).to_string().into_bytes()
}

#[test]
fn arcgis_token_form_percent_encodes_the_password_in_the_body_only() {
    let form = form("ana lyst", "p&= +%");
    assert_eq!(
        form.expose(),
        "f=json&username=ana%20lyst&password=p%26%3D%20%2B%25&client=requestip&expiration=60",
        "exactly these fields, in this order, every value encoded"
    );
    // Neither the endpoint nor a Debug print can reach the password.
    assert_eq!(format!("{form:?}"), "Secret(<redacted>)");
}

#[test]
fn arcgis_token_answers_are_read_strictly() {
    let token = from_answer(200, &answer("test-token", FUTURE), Some(NOW)).ok().unwrap();
    assert_eq!(token.token.expose(), "test-token");
    assert_eq!(token.expires_at, Some(FUTURE));
    // Tokens carry letters, digits and - _ . ~ + / =.
    assert!(from_answer(200, &answer("Ab-_.~+/=09", FUTURE), Some(NOW)).is_ok());

    let refused = |status: u16, body: &[u8], now: Option<i64>| from_answer(status, body, now).err().unwrap();
    // The server's error is never passed on, even when it echoes what was sent.
    let echoed =
        br#"{"error":{"code":400,"message":"Unable to generate token for user p&= +% with password hunter2","details":["hunter2"]}}"#;
    let message = refused(200, echoed, Some(NOW));
    assert_eq!(message, REFUSED);
    assert!(!message.contains("hunter2"));
    // An error status says only its number.
    assert_eq!(refused(500, echoed, Some(NOW)), "The server refused the token request (HTTP 500).");
    assert_eq!(refused(302, b"", Some(NOW)), "The server refused the token request (HTTP 302).");
    // OAuth's answer is another protocol; so is a JWT with no expiry, or any shape but the token's.
    for body in [
        br#"{"access_token":"test-token","expires_in":1800}"#.as_slice(),
        br#"{"token":"test-token"}"#,
        br#"{"token":"test-token","expires":"4102444800000"}"#,
        br#"{"token":"test-token","expires":4102444800000.5}"#,
        br#"{"token":"test-token","expires":null}"#,
        br#"{"token":"","expires":4102444800000}"#,
        br#"{"token":"a b","expires":4102444800000}"#,
        br#"{"token":"a\nb","expires":4102444800000}"#,
        br#"{"token":7,"expires":4102444800000}"#,
        br#"[]"#,
        br#"not json"#,
        b"",
    ] {
        assert_eq!(refused(200, body, Some(NOW)), BAD_ANSWER, "{}", String::from_utf8_lossy(body));
    }
    // An expiry in the past, or at the moment, is not a token; an expiry is never made up.
    assert_eq!(refused(200, &answer("test-token", NOW - 1), Some(NOW)), BAD_ANSWER);
    assert_eq!(refused(200, &answer("test-token", NOW), Some(NOW)), BAD_ANSWER);
    assert_eq!(refused(200, &answer("test-token", FUTURE), None), NO_CLOCK);
    assert_eq!(refused(200, &answer("test-token", 0), Some(NOW)), BAD_ANSWER);
    assert_eq!(refused(200, &answer(&"a".repeat(MAX_TOKEN + 1), FUTURE), Some(NOW)), BAD_ANSWER);
}

#[test]
fn arcgis_token_payload_is_exact() {
    let c = Credential::new("test-token", Some(FUTURE)).ok().unwrap();
    assert_eq!(c.payload().expose(), format!(r#"{{"expiresAt":{FUTURE},"token":"test-token","version":1}}"#));
    let back = Credential::parse(c.payload().expose()).ok().unwrap();
    assert_eq!((back.token.expose(), back.expires_at), ("test-token", Some(FUTURE)));
    let unknown = Credential::new("test-token", None).ok().unwrap();
    assert_eq!(Credential::parse(unknown.payload().expose()).ok().unwrap().expires_at, None);

    for bad in [
        r#"{"version":1,"token":"test-token"}"#,
        r#"{"version":1,"token":"test-token","expiresAt":null,"extra":1}"#,
        r#"{"version":2,"token":"test-token","expiresAt":null}"#,
        r#"{"version":"1","token":"test-token","expiresAt":null}"#,
        r#"{"version":1,"token":"","expiresAt":null}"#,
        r#"{"version":1,"token":"a b","expiresAt":null}"#,
        r#"{"version":1,"token":"test-token","expiresAt":0}"#,
        r#"{"version":1,"token":"test-token","expiresAt":-5}"#,
        r#"{"version":1,"token":"test-token","expiresAt":"5"}"#,
        r#"{"version":1,"token":"test-token","expiresAt":1.5}"#,
        r#"{"version":1,"token":"test-token","expiresAt":18446744073709551615}"#,
        r#"[1,"test-token",null]"#,
        "test-token",
        "",
    ] {
        assert!(Credential::parse(bad).is_err(), "{bad}");
    }
    assert!(Credential::new(&"a".repeat(MAX_TOKEN), None).is_ok());
    assert!(Credential::new(&"a".repeat(MAX_TOKEN + 1), None).is_err());
    assert!(Credential::new("é", None).is_err());
}

#[test]
fn arcgis_token_expiry_follows_an_explicit_clock() {
    let c = Credential::new("test-token", Some(NOW)).ok().unwrap();
    assert!(!c.is_expired(Some(NOW - 1)));
    assert!(c.is_expired(Some(NOW)));
    assert!(c.is_expired(Some(NOW + 1)));
    // A clock that can't be read refuses an expiring token and still lets one with no expiry be.
    assert!(c.is_expired(None));
    assert!(!Credential::new("test-token", None).ok().unwrap().is_expired(None));
}

#[test]
fn arcgis_token_endpoints_stay_on_the_connection_origin() {
    let origin = "https://gis.example.test";
    for good in [
        "https://gis.example.test/arcgis/tokens/generateToken",
        "HTTPS://GIS.example.test:443/arcgis/tokens/generateToken",
        "https://gis.example.test/tokens",
    ] {
        assert_eq!(check_endpoint(good, origin), Ok(()), "{good}");
    }
    for bad in [
        "http://gis.example.test/arcgis/tokens/generateToken",
        "https://other.example.test/arcgis/tokens/generateToken",
        "https://gis.example.test:8443/arcgis/tokens/generateToken",
        "https://user:secret@gis.example.test/arcgis/tokens/generateToken",
        "https://gis.example.test/arcgis/tokens/generateToken?f=json",
        "https://gis.example.test/arcgis/tokens/generateToken#frag",
        "https://gis.example.test/a b",
        "/arcgis/tokens/generateToken",
        "gis.example.test/arcgis/tokens/generateToken",
        "",
    ] {
        assert_eq!(check_endpoint(bad, origin), Err(BAD_ENDPOINT), "{bad}");
    }
    assert_eq!(check_endpoint(&format!("https://gis.example.test/{}", "a".repeat(MAX_ENDPOINT)), origin), Err(BAD_ENDPOINT));
}

#[test]
fn arcgis_token_generate_refuses_plain_http_and_calls_done_once() {
    let (sent, received) = std::sync::mpsc::channel();
    let done: Done = Box::new(move |r| {
        let _ = sent.send(r.err());
    });
    generate("http://gis.example.test/tokens".into(), Secret::new("f=json"), Arc::new(|| true), done);
    assert_eq!(received.recv().unwrap().as_deref(), Some(BAD_ENDPOINT));
    assert!(received.try_recv().is_err());
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc;
    use std::time::Duration;

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .max_redirects(0)
            .max_idle_connections(0)
            .build()
            .into()
    }

    /// Reads a request whole, body included.
    fn read_request(stream: &mut std::net::TcpStream) -> String {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut request = Vec::new();
        let mut buf = [0u8; 2048];
        while let Ok(n) = stream.read(&mut buf) {
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request).into_owned();
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length = head
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
                    .unwrap_or(0);
                if body.len() >= length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&request).into_owned()
    }

    /// A server on this computer answering each connection with the next of `answers`; the
    /// requests it read come out of the receiver.
    fn serve(answers: Vec<Vec<u8>>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            for answer in answers {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let _ = sent.send(read_request(&mut stream));
                let _ = stream.write_all(&answer);
            }
        });
        (format!("http://{address}/arcgis/tokens/generateToken"), received)
    }

    fn ok(body: &[u8]) -> Vec<u8> {
        let mut out =
            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                .into_bytes();
        out.extend_from_slice(body);
        out
    }

    fn live() -> Still {
        Arc::new(|| true)
    }

    #[test]
    fn arcgis_token_native_posts_the_form_and_reads_the_token() {
        let (url, requests) = serve(vec![ok(&answer("test-token", FUTURE))]);
        let body = form("analyst", "p&= +%");
        let credential = super::super::native::run(&agent(), &url, &body, &live()).ok().unwrap();
        assert_eq!((credential.token.expose(), credential.expires_at), ("test-token", Some(FUTURE)));
        let request = requests.recv().unwrap();
        let (head, sent) = request.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("POST /arcgis/tokens/generateToken HTTP/1.1"), "{head}");
        assert!(head.to_ascii_lowercase().contains("content-type: application/x-www-form-urlencoded"));
        assert_eq!(sent, "f=json&username=analyst&password=p%26%3D%20%2B%25&client=requestip&expiration=60");
        // The password is in the body alone: not the address, not a header.
        for part in [url.as_str(), head] {
            assert!(!part.contains("p%26") && !part.contains("p&="), "{part}");
        }
        let lower = head.to_ascii_lowercase();
        assert!(!lower.contains("cookie") && !lower.contains("authorization") && !lower.contains("referer"), "{head}");
    }

    #[test]
    fn arcgis_token_native_never_follows_a_redirect() {
        let (target, targets) = serve(vec![ok(&answer("test-token", FUTURE))]);
        for status in ["302 Found", "307 Temporary Redirect", "308 Permanent Redirect"] {
            // A redirect to the same origin or to another is the same refusal: the form is not replayed.
            let redirect =
                format!("HTTP/1.1 {status}\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes();
            let (url, _) = serve(vec![redirect]);
            let message = super::super::native::run(&agent(), &url, &form("analyst", "secret-password"), &live()).err().unwrap();
            assert!(!message.contains("secret-password") && !message.contains(&target), "{message}");
        }
        assert!(targets.recv_timeout(Duration::from_millis(300)).is_err(), "the redirect target was asked");
    }

    #[test]
    fn arcgis_token_native_refuses_errors_oversized_and_malformed_answers_without_echoing() {
        let echo = br#"{"error":{"code":400,"message":"bad password secret-password for analyst"}}"#;
        let mut large = br#"{"token":"test-token","expires":4102444800000,"pad":""#.to_vec();
        large.extend(std::iter::repeat_n(b'a', MAX_ANSWER));
        large.extend_from_slice(br#""}"#);
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (ok(echo), REFUSED),
            (
                format!("HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", echo.len())
                    .into_bytes()
                    .into_iter()
                    .chain(echo.iter().copied())
                    .collect(),
                "The server refused the token request (HTTP 500).",
            ),
            (ok(&large), TOO_LARGE),
            (ok(b"<html>secret-password</html>"), BAD_ANSWER),
            (ok(&answer("test-token", PAST)), BAD_ANSWER),
        ];
        for (reply, expected) in cases {
            let (url, _) = serve(vec![reply]);
            let message = super::super::native::run(&agent(), &url, &form("analyst", "secret-password"), &live()).err().unwrap();
            assert_eq!(message, expected);
            assert!(!message.contains("secret-password"));
        }
        // Nobody listening: a fixed sentence, not the transport's text.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let message = super::super::native::run(&agent(), &format!("http://{closed}/tokens"), &form("analyst", "secret-password"), &live())
            .err()
            .unwrap();
        assert_eq!(message, FAILED);
    }

    #[test]
    fn arcgis_token_native_cancellation_stops_before_sending_and_discards_a_late_answer() {
        // Cancelled before it is prepared: nothing is sent.
        let (url, requests) = serve(vec![ok(&answer("test-token", FUTURE))]);
        let cancelled: Still = Arc::new(|| false);
        let message = super::super::native::run(&agent(), &url, &form("analyst", "secret-password"), &cancelled).err().unwrap();
        assert_eq!(message, CANCELLED);
        assert!(requests.recv_timeout(Duration::from_millis(300)).is_err(), "a cancelled request was sent");

        // Cancelled after it was sent: the answer that comes is discarded.
        let (url, requests) = serve(vec![ok(&answer("test-token", FUTURE))]);
        let flag = Arc::new(AtomicBool::new(true));
        let looks = Arc::new(AtomicUsize::new(0));
        let (seen, cancel) = (flag.clone(), flag.clone());
        let counted = looks.clone();
        // The first two looks (before preparing, immediately before sending) pass; the request is
        // then sent, and what looks next finds it cancelled, as the server's answer arrives.
        let still: Still = Arc::new(move || {
            if counted.fetch_add(1, SeqCst) >= 2 {
                cancel.store(false, SeqCst);
            }
            seen.load(SeqCst)
        });
        let message = super::super::native::run(&agent(), &url, &form("analyst", "secret-password"), &still).err().unwrap();
        assert_eq!(message, CANCELLED);
        assert!(requests.recv_timeout(Duration::from_secs(5)).is_ok(), "the request was sent");
        assert!(!flag.load(SeqCst));
    }

    #[test]
    fn arcgis_token_generate_calls_done_once_with_the_token() {
        let (url, _) = serve(vec![ok(&answer("test-token", FUTURE))]);
        let (sent, received) = mpsc::channel();
        let done: Done = Box::new(move |r| {
            let _ = sent.send(r.map(|c| (c.token.expose().to_string(), c.expires_at)));
        });
        // The built-in transport is https-only; a plain address is the test server's, so it is
        // run directly, and `generate` is shown to refuse it above.
        let still = live();
        let slot = Arc::new(Mutex::new(Some(done)));
        super::super::native::finish(&slot, super::super::native::run(&agent(), &url, &form("analyst", "pw"), &still));
        super::super::native::finish(&slot, Err("again".into()));
        assert_eq!(received.recv().unwrap(), Ok(("test-token".to_string(), Some(FUTURE))));
        assert!(received.try_recv().is_err(), "done is called once");
    }
}
