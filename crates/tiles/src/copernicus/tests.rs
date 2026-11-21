//! Tested against a fake Keycloak on this computer: no request leaves it, and no real account
//! or password is used.
#![cfg(not(target_arch = "wasm32"))]

use super::*;
use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

/// Serves every connection with what `answer` gives for the whole request (head and body),
/// sending each request to the receiver.
fn serve(answer: impl Fn(&str) -> String + Send + 'static) -> (String, Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (sent, received) = channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let request = read_request(&mut stream);
            let reply = answer(&request);
            let _ = sent.send(request);
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (format!("http://{addr}"), received)
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut request = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = stream.read(&mut buf) {
        if n == 0 {
            break;
        }
        request.extend_from_slice(&buf[..n]);
        let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
        let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
        let length = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
        if request.len() >= end + 4 + length {
            break;
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}

fn json_answer(status: u16, body: &str) -> String {
    format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn grant(access: &str, refresh: &str, expires_in: i64, refresh_expires_in: i64) -> String {
    format!(
        r#"{{"access_token":"{access}","expires_in":{expires_in},"refresh_expires_in":{refresh_expires_in},"refresh_token":"{refresh}","token_type":"Bearer"}}"#
    )
}

const DATA: &str = "https://download.dataspace.copernicus.eu/odata/v1/Products(abc)/Nodes(p)/Nodes(f.tif)/$value";
/// Synthetic credentials: never a real account's.
const USER: &str = "synthetic.user@example.invalid";
const PASSWORD: &str = "synthetic pa&ss=word%";

#[test]
fn the_token_goes_to_cdse_data_hosts_only() {
    for url in [DATA, "https://zipper.dataspace.copernicus.eu/odata/v1/Products(abc)/$value", "HTTPS://Download.DataSpace.Copernicus.EU./x"]
    {
        assert!(is_data_host(url), "{url}");
    }
    for url in [
        "http://download.dataspace.copernicus.eu/x",
        "https://download.dataspace.copernicus.eu:8443/x",
        "https://user@download.dataspace.copernicus.eu/x",
        "https://identity.dataspace.copernicus.eu/auth/realms/CDSE/protocol/openid-connect/token",
        "https://stac.dataspace.copernicus.eu/v1",
        "https://download.dataspace.copernicus.eu.example.com/x",
        "https://evil-download.dataspace.copernicus.eu/x",
        "https://x.download.dataspace.copernicus.eu/x",
        "https://example.com/download.dataspace.copernicus.eu/x",
        "https://example.com/?u=https://download.dataspace.copernicus.eu/x",
        "s3://eodata/Sentinel-2/x.tif",
    ] {
        assert!(!is_data_host(url), "{url}");
        assert_eq!(authorization(url), None, "{url}");
    }
}

#[test]
fn the_sign_in_form_is_encoded_and_checked() {
    let body = login_body(" a b@c.d ", "p&q=r%", Some(" 123456 ")).unwrap();
    assert_eq!(body, "grant_type=password&client_id=cdse-public&username=a%20b%40c.d&password=p%26q%3Dr%25&totp=123456");
    assert!(!login_body("u", "p", None).unwrap().contains("totp"));
    for (u, p, t) in [("", "p", None), ("u", "", None), ("u", "p\n", None), ("u", "p", Some("12ab56")), ("u", "p", Some("1"))] {
        let e = login_body(u, p, t).unwrap_err();
        assert!(!e.contains("p\n") && !e.contains("12ab56"), "nothing given is quoted back: {e}");
    }
    assert!(login_body("u", &"x".repeat(MAX_FIELD + 1), None).is_err());
}

#[test]
fn a_sign_in_exchanges_the_password_for_tokens_at_the_token_endpoint() {
    let _turn = crate::cache::turn();
    logout();
    let (base, asked) = serve(|_| json_answer(200, &grant("access-one", "refresh-one", 1800, 3600)));
    let endpoint = format!("{base}/auth/realms/CDSE/protocol/openid-connect/token");
    let info = login_at(&endpoint, USER, PASSWORD, None).unwrap();
    let request = asked.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(request.starts_with("POST /auth/realms/CDSE/protocol/openid-connect/token HTTP/1.1\r\n"), "{request}");
    assert!(request.to_ascii_lowercase().contains("content-type: application/x-www-form-urlencoded"), "{request}");
    assert!(
        request.ends_with(
            "grant_type=password&client_id=cdse-public&username=synthetic.user%40example.invalid&password=synthetic%20pa%26ss%3Dword%25"
        ),
        "{request}"
    );
    assert!(!request.to_ascii_lowercase().contains("authorization:"), "the sign-in host never gets a token: {request}");
    assert_eq!(info.username, USER);
    assert!((info.expires_at - now_secs() - 1800).abs() <= 5);
    assert!(info.renews_until.is_some_and(|t| (t - now_secs() - 3600).abs() <= 5) && info.renews);
    assert_eq!(authorization(DATA).as_deref(), Some("Bearer access-one"));
    assert_eq!(authorization(&endpoint), None);
    assert_eq!(authorization(TOKEN_URL), None);
    // Fresh: nothing is renewed.
    assert_eq!(authorization_fresh_using(DATA, None, |_, _, _| panic!("not due")).as_deref(), Some("Bearer access-one"));
    assert!(logout());
    assert_eq!(authorization(DATA), None);
    assert!(!expired(), "a sign-out isn't an expiry");
}

#[test]
fn a_refused_sign_in_says_why_without_the_password_and_keeps_the_one_before() {
    let _turn = crate::cache::turn();
    logout();
    let (base, _asked) = serve(|request| {
        if request.contains("password=right") {
            json_answer(200, &grant("access-kept", "refresh-kept", 1800, 3600))
        } else {
            json_answer(401, r#"{"error":"invalid_grant","error_description":"Invalid user credentials"}"#)
        }
    });
    login_at(&base, USER, "right", None).unwrap();
    let e = login_at(&base, USER, PASSWORD, None).unwrap_err();
    assert_eq!(e, "Copernicus Data Space refused the sign-in: Invalid user credentials");
    assert!(!e.contains(PASSWORD) && !e.contains("synthetic%20pa"), "{e}");
    assert_eq!(authorization(DATA).as_deref(), Some("Bearer access-kept"));
    // An answer that isn't a token (a proxy's page) is refused without quoting it.
    let (html, _) = serve(|_| "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\n<html>x</ht>".into());
    let e = login_at(&html, USER, "right", None).unwrap_err();
    assert!(e.contains("not a token") && !e.contains("<html>"), "{e}");
    let (no_lifetime, _) = serve(|_| json_answer(200, r#"{"access_token":"a","refresh_token":"r"}"#));
    assert!(login_at(&no_lifetime, USER, "right", None).unwrap_err().contains("without its lifetime"));
    let (down, _) = serve(|_| json_answer(503, "{}"));
    assert!(login_at(&down, USER, "right", None).unwrap_err().contains("HTTP 503"));
    assert_eq!(authorization(DATA).as_deref(), Some("Bearer access-kept"));
    logout();
}

/// The web app sends the sign-in itself and lands its answer: kept as a sign-in made here is,
/// renewed at CDSE's token endpoint, and a refusal or a server error leaves the one before.
#[test]
fn a_sign_in_answer_sent_from_the_browser_is_kept_like_one_made_here() {
    let _turn = crate::cache::turn();
    logout();
    let info = login_answer(&sending(), &format!(" {USER} "), 200, grant("web-access", "web-refresh", 30, 3600).as_bytes()).unwrap();
    assert_eq!(info.username, USER);
    assert_eq!(authorization(DATA).as_deref(), Some("Bearer web-access"));
    let due = renewal_due().unwrap();
    assert_eq!(due.endpoint, TOKEN_URL);
    assert!(due.body.contains("refresh_token=web-refresh"), "{}", due.body);
    let e =
        login_answer(&sending(), USER, 401, br#"{"error":"invalid_grant","error_description":"Invalid user credentials"}"#).unwrap_err();
    assert_eq!(e, "Copernicus Data Space refused the sign-in: Invalid user credentials");
    let e = login_answer(&sending(), USER, 502, b"<html>bad gateway</html>").unwrap_err();
    assert!(e.contains("HTTP 502") && !e.contains("<html>"), "{e}");
    let e = login_answer(&sending(), USER, 200, b"<html>a proxy's page</html>").unwrap_err();
    assert!(e.contains("not a token") && !e.contains("<html>"), "{e}");
    assert_eq!(authorization(DATA).as_deref(), Some("Bearer web-access"), "a failed sign-in leaves the one before");
    assert!(!is_remembered(), "a sign-in from the browser is never remembered");
    logout();
    assert_eq!(authorization(DATA), None);
    // A sign-out while a sign-in is on its way wins: the late answer isn't kept.
    let sent = sending();
    logout();
    let e = login_answer(&sent, USER, 200, grant("late-access", "late-refresh", 30, 3600).as_bytes()).unwrap_err();
    assert!(e.contains("Signed out while"), "{e}");
    assert_eq!(authorization(DATA), None, "a sign-in answered after a sign-out isn't kept");
    assert!(signed_in().is_none());
    // The web app signs in from its panel: its refusal sends the user there.
    let web = refused_message_in(DATA, true);
    assert!(
        web.ends_with(
            "f.tif: Copernicus Data Space asks for a sign-in for this file: sign in with a free account in Home › Add › Web Data › Copernicus Data Space…"
        ),
        "{web}"
    );
    assert!(refused_message(DATA).ends_with("sign in with a free account (copernicus.login)"), "{}", refused_message(DATA));
}

#[test]
fn the_access_token_is_renewed_before_it_expires() {
    let _turn = crate::cache::turn();
    logout();
    let (base, asked) = serve(|request| {
        if request.contains("grant_type=password") {
            // Expires within the margin: due for renewal at once.
            json_answer(200, &grant("access-old", "refresh-old", 30, 3600))
        } else {
            json_answer(200, &grant("access-new", "refresh-new", 1800, 3600))
        }
    });
    login_at(&base, USER, PASSWORD, None).unwrap();
    let _ = asked.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(renewal_due().is_some());
    // Not for an address the token doesn't go to.
    assert_eq!(authorization_fresh("https://example.com/x.tif"), None);
    assert!(asked.recv_timeout(Duration::from_millis(200)).is_err(), "nothing renewed for another host");
    assert_eq!(authorization_fresh(DATA).as_deref(), Some("Bearer access-new"));
    let renewal = asked.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(renewal.ends_with("grant_type=refresh_token&client_id=cdse-public&refresh_token=refresh-old"), "{renewal}");
    assert!(!renewal.contains("password") && !renewal.contains("access-old"), "{renewal}");
    assert!(renewal_due().is_none());
    // The new refresh token renews the next time.
    {
        let mut st = state();
        let s = st.session.as_mut().unwrap();
        s.expires_at = now_secs() + 10;
    }
    assert_eq!(authorization_fresh(DATA).as_deref(), Some("Bearer access-new"));
    assert!(asked.recv_timeout(Duration::from_secs(5)).unwrap().ends_with("refresh_token=refresh-new"));
    logout();
}

#[test]
fn a_refused_renewal_ends_the_sign_in_and_an_unreachable_host_keeps_it() {
    let _turn = crate::cache::turn();
    logout();
    let (base, _asked) = serve(|request| {
        if request.contains("grant_type=password") {
            json_answer(200, &grant("access-old", "refresh-old", 30, 3600))
        } else {
            json_answer(400, r#"{"error":"invalid_grant","error_description":"Token is not active"}"#)
        }
    });
    login_at(&base, USER, PASSWORD, None).unwrap();
    // Unreachable, then a server error: kept, the still-valid token sent, renewal held off.
    assert_eq!(authorization_fresh_using(DATA, None, |_, _, _| Err("refused".into())).as_deref(), Some("Bearer access-old"));
    assert!(renewal_due().is_none(), "held off after an unreachable host");
    state().session.as_mut().unwrap().retry_at = 0;
    assert_eq!(authorization_fresh_using(DATA, None, |_, _, _| Ok((502, b"<html>".to_vec()))).as_deref(), Some("Bearer access-old"));
    // A rate limit, a timeout or a proxy's 403 page says nothing of the refresh token: kept too,
    // and the account is the same one (a renewal that failed doesn't change it).
    let identity = account_identity().unwrap();
    let malformed = br#"{"error":"invalid_request","error_description":"Missing parameter"}"#;
    for (status, body) in [
        (429, &b"<html>busy</html>"[..]),
        (408, b"<html>busy</html>"),
        (403, b"<html>busy</html>"),
        (400, b"<html>bad</html>"),
        (400, malformed),
        (401, b"<html>sign in</html>"),
    ] {
        state().session.as_mut().unwrap().retry_at = 0;
        let answer = authorization_fresh_using(DATA, None, move |_, _, _| Ok((status, body.to_vec())));
        assert_eq!(answer.as_deref(), Some("Bearer access-old"), "HTTP {status}");
        assert!(signed_in().is_some() && !expired(), "HTTP {status}");
    }
    assert!(identity.is_current(), "a failed renewal keeps the identity");
    // Its token expired while the sign-in host didn't answer: the message says to try again.
    state().session.as_mut().unwrap().expires_at = now_secs() - 1;
    assert!(refused_message(DATA).contains("expired and couldn't be renewed yet"), "{}", refused_message(DATA));
    state().session.as_mut().unwrap().retry_at = 0;
    // Refused: ended, and the message says so.
    assert_eq!(authorization_fresh(DATA), None);
    assert!(signed_in().is_none() && expired());
    assert!(refused_message(DATA).contains("f.tif: the Copernicus Data Space sign-in expired"), "{}", refused_message(DATA));
    logout();
    assert!(!expired());
    assert!(refused_message(DATA).contains("asks for a sign-in"));
}

#[test]
fn a_sign_in_ends_when_both_tokens_have_expired() {
    let _turn = crate::cache::turn();
    logout();
    let (base, asked) = serve(|_| json_answer(200, &grant("access-old", "refresh-old", 30, 3600)));
    login_at(&base, USER, PASSWORD, None).unwrap();
    let _ = asked.recv_timeout(Duration::from_secs(5)).unwrap();
    {
        let mut st = state();
        let s = st.session.as_mut().unwrap();
        s.expires_at = now_secs() - 1;
        s.refresh_expires_at = now_secs() - 1;
    }
    assert_eq!(authorization_fresh(DATA), None);
    assert!(asked.recv_timeout(Duration::from_millis(200)).is_err(), "an expired refresh token isn't sent");
    assert!(signed_in().is_none() && expired());
    logout();
}

#[test]
fn a_renewal_lands_only_on_the_sign_in_it_was_for() {
    let _turn = crate::cache::turn();
    logout();
    let (base, _asked) = serve(|request| {
        let access = if request.contains("username=first") { "access-first" } else { "access-second" };
        json_answer(200, &grant(access, access, 30, 3600))
    });
    login_at(&base, "first", PASSWORD, None).unwrap();
    let due = renewal_due().unwrap();
    login_at(&base, "second", PASSWORD, None).unwrap();
    renewal_done(&due, Ok((200, grant("stale", "stale", 1800, 3600).as_bytes())));
    assert_eq!(authorization(DATA).as_deref(), Some("Bearer access-second"));
    let identity = account_identity().unwrap();
    assert!(identity.is_current());
    renewal_done(&renewal_due().unwrap(), Ok((200, grant("renewed", "renewed", 1800, 3600).as_bytes())));
    assert_eq!(authorization(DATA).as_deref(), Some("Bearer renewed"));
    assert!(!identity.is_current(), "a renewal changes the identity");
    logout();
}

/// A credential store in memory for the test, and a fake token endpoint for `restore`; both
/// taken away at the end, even when the test fails.
struct Remembering(Arc<rustgis_data::secrets::MemorySecrets>);

impl Remembering {
    fn new(endpoint: &str) -> Remembering {
        let store = Arc::new(rustgis_data::secrets::MemorySecrets::new());
        set_secret_store(Some(store.clone()));
        *TEST_TOKEN_URL.lock().unwrap_or_else(|e| e.into_inner()) = Some(endpoint.to_string());
        Remembering(store)
    }

    fn kept(&self) -> Option<String> {
        self.0.load(ACCOUNT).unwrap().map(|s| s.expose().to_string())
    }
}

impl Drop for Remembering {
    fn drop(&mut self) {
        set_secret_store(None);
        *TEST_TOKEN_URL.lock().unwrap_or_else(|e| e.into_inner()) = None;
        logout();
    }
}

#[test]
fn a_remembered_sign_in_signs_in_again_when_none_is_kept_and_a_sign_out_deletes_it() {
    let _turn = crate::cache::turn();
    logout();
    let (base, asked) = serve(|_| json_answer(200, &grant("access-one", "refresh-one", 1800, 3600)));
    let store = Remembering::new(&base);
    let (info, warning) = login_remembering_at(&base, USER, PASSWORD, None, true).unwrap();
    assert_eq!((info.username.as_str(), warning), (USER, None));
    let _ = asked.recv_timeout(Duration::from_secs(5)).unwrap();
    // The username and password are in the store, and only there: never a token.
    let kept = store.kept().unwrap();
    assert_eq!(read_remembered(&kept), Some((USER.to_string(), PASSWORD.to_string())));
    assert!(!kept.contains("access-one") && !kept.contains("refresh-one"), "{kept}");
    assert!(is_remembered());
    // A new run: nothing kept for the session, the store not read yet.
    set_secret_store(Some(store.0.clone()));
    logout();
    assert_eq!(is_remembered_now(), Some(false), "a frame's read never loads the store");
    assert_eq!(authorization_fresh_using("https://example.com/x.tif", None, |_, _, _| panic!("no renewal")), None);
    assert!(asked.recv_timeout(Duration::from_millis(200)).is_err(), "another host doesn't sign in");
    assert_eq!(authorization_fresh_using(DATA, None, |_, _, _| panic!("no renewal")).as_deref(), Some("Bearer access-one"));
    let request = asked.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(request.ends_with("&username=synthetic.user%40example.invalid&password=synthetic%20pa%26ss%3Dword%25"), "{request}");
    assert_eq!(is_remembered_now(), Some(true), "read once the sign-in has used it");
    // Both tokens expired: it signs in again rather than ending.
    {
        let mut st = state();
        let s = st.session.as_mut().unwrap();
        s.expires_at = now_secs() - 1;
        s.refresh_expires_at = now_secs() - 1;
    }
    assert_eq!(authorization_fresh(DATA).as_deref(), Some("Bearer access-one"));
    assert!(asked.recv_timeout(Duration::from_secs(5)).unwrap().contains("grant_type=password"));
    assert!(!expired());
    // A sign-out deletes it: nothing signs in again.
    assert_eq!(forget(), Ok(()));
    assert!(store.kept().is_none() && !is_remembered() && signed_in().is_none());
    assert_eq!(authorization_fresh(DATA), None);
    assert!(asked.recv_timeout(Duration::from_millis(200)).is_err(), "a forgotten sign-in isn't used");
}

#[test]
fn a_sign_in_not_to_be_remembered_deletes_the_one_before_and_two_factor_is_never_kept() {
    let _turn = crate::cache::turn();
    logout();
    let (base, _asked) = serve(|_| json_answer(200, &grant("access", "refresh", 1800, 3600)));
    // No store (the command line without saved secrets): kept for the session only.
    set_secret_store(None);
    assert_eq!(login_remembering_at(&base, USER, PASSWORD, None, true).unwrap().1, Some(NO_STORE));
    assert_eq!(login_remembering_at(&base, USER, PASSWORD, None, false).unwrap().1, None);
    let store = Remembering::new(&base);
    login_remembering_at(&base, USER, PASSWORD, None, true).unwrap();
    assert!(store.kept().is_some());
    assert_eq!(login_remembering_at(&base, USER, PASSWORD, None, false).unwrap().1, None);
    assert!(store.kept().is_none() && !is_remembered());
    login_remembering_at(&base, USER, PASSWORD, None, true).unwrap();
    assert_eq!(login_remembering_at(&base, USER, PASSWORD, Some("123456"), true).unwrap().1, Some(TWO_FACTOR));
    assert!(store.kept().is_none(), "a two-factor account's password isn't remembered, and the one before is gone");
    assert!(signed_in().is_some(), "kept for the session");
    // A refused sign-in leaves the one remembered before.
    login_remembering_at(&base, USER, PASSWORD, None, true).unwrap();
    let (refusing, _) = serve(|_| json_answer(401, r#"{"error":"invalid_grant","error_description":"Invalid user credentials"}"#));
    assert!(login_remembering_at(&refusing, USER, "wrong", None, false).is_err());
    assert!(store.kept().is_some());
    // A store that refuses, not read yet: kept for the session, and a password an earlier run
    // remembered may still be there, which is said first, even with a one-time code.
    use rustgis_data::secrets::{MemorySecrets, SecretError};
    for totp in [None, Some("123456")] {
        set_secret_store(Some(Arc::new(MemorySecrets::failing(SecretError::Denied))));
        assert_eq!(login_remembering_at(&base, USER, PASSWORD, totp, true).unwrap().1, Some(NOT_FORGOTTEN));
        assert!(!is_remembered(), "not claimed remembered: nothing was found");
    }
    // Read and refused at launch: still may hold one, said first; never claimed remembered, and a
    // sign-out says it couldn't delete it.
    set_secret_store(Some(Arc::new(MemorySecrets::failing(SecretError::Denied))));
    assert!(!is_remembered());
    assert_eq!(restore(), Restored::Nothing);
    assert_eq!(login_remembering_at(&base, USER, PASSWORD, Some("123456"), true).unwrap().1, Some(NOT_FORGOTTEN));
    assert_eq!(forget(), Err(NOT_FORGOTTEN));
    assert!(!is_remembered());
    // No credential store on this system: nothing could have been kept.
    set_secret_store(Some(Arc::new(MemorySecrets::failing(SecretError::Unavailable("none".into())))));
    assert_eq!(login_remembering_at(&base, USER, PASSWORD, None, true).unwrap().1, Some(NOT_REMEMBERED));
    assert!(signed_in().is_some());
}

#[test]
fn a_refused_remembered_sign_in_is_deleted_and_an_unreachable_host_is_tried_again_later() {
    let _turn = crate::cache::turn();
    logout();
    let answer = Arc::new(Mutex::new(json_answer(503, "{}")));
    let (base, asked) = {
        let answer = answer.clone();
        serve(move |_| answer.lock().unwrap().clone())
    };
    let store = Remembering::new(&base);
    store.0.save(ACCOUNT, &remembered_value(USER, PASSWORD)).unwrap();
    // Down: kept, and not tried again at once.
    assert_eq!(restore(), Restored::Nothing);
    let _ = asked.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(restore(), Restored::Nothing);
    assert!(asked.recv_timeout(Duration::from_millis(200)).is_err(), "held off after an unreachable host");
    assert!(store.kept().is_some() && is_remembered() && !expired());
    // A refused client says nothing of the password: kept, tried again later.
    *lock(&REMEMBERED.known) = Known::Kept { retry_at: 0 };
    *answer.lock().unwrap() = json_answer(401, r#"{"error":"invalid_client","error_description":"Invalid client credentials"}"#);
    assert_eq!(restore(), Restored::Nothing);
    let _ = asked.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(store.kept().is_some() && !expired());
    // Refused (the password was changed): deleted, and the user is told to sign in again.
    *lock(&REMEMBERED.known) = Known::Kept { retry_at: 0 };
    *answer.lock().unwrap() = json_answer(401, r#"{"error":"invalid_grant","error_description":"Invalid user credentials"}"#);
    assert_eq!(restore(), Restored::Refused);
    assert!(store.kept().is_none() && !is_remembered() && expired());
    assert!(refused_message(DATA).contains("sign-in expired: sign in again"), "{}", refused_message(DATA));
    // Damaged: deleted, never sent, and said: the sign-in ended, sign in again.
    logout();
    assert!(!expired());
    store.0.save(ACCOUNT, &rustgis_data::secrets::Secret::new("not json")).unwrap();
    set_secret_store(Some(store.0.clone()));
    let _ = asked.try_iter().count();
    assert_eq!(restore(), Restored::Damaged);
    assert!(asked.recv_timeout(Duration::from_millis(200)).is_err());
    assert!(store.kept().is_none() && !is_remembered() && expired());
    assert!(refused_message(DATA).contains("sign-in expired: sign in again"), "{}", refused_message(DATA));
    // A clean one gives no such report.
    logout();
    store.0.save(ACCOUNT, &remembered_value(USER, PASSWORD)).unwrap();
    set_secret_store(Some(store.0.clone()));
    *answer.lock().unwrap() = json_answer(200, &grant("access", "refresh", 1800, 3600));
    assert_eq!(restore(), Restored::SignedIn);
    assert!(!expired());
}

/// A store that reads and saves but refuses to delete (a policy-locked keychain).
struct Undeletable(rustgis_data::secrets::MemorySecrets);

impl SecretStore for Undeletable {
    fn load(&self, account: &str) -> Result<Option<rustgis_data::secrets::Secret>, SecretError> {
        self.0.load(account)
    }
    fn save(&self, account: &str, value: &rustgis_data::secrets::Secret) -> Result<(), SecretError> {
        self.0.save(account, value)
    }
    fn delete(&self, _account: &str) -> Result<(), SecretError> {
        Err(SecretError::Denied)
    }
}

#[test]
fn a_remembered_sign_in_that_couldnt_be_deleted_is_never_signed_in_with() {
    let _turn = crate::cache::turn();
    logout();
    let (base, asked) = serve(|_| json_answer(200, &grant("access", "refresh", 1800, 3600)));
    let _clean = Remembering::new(&base);
    let store = Arc::new(Undeletable(rustgis_data::secrets::MemorySecrets::new()));
    store.save(ACCOUNT, &remembered_value("earlier.user@example.invalid", "earlier")).unwrap();
    // A new run, the store unread: signing in without `remember` can't delete the earlier one.
    set_secret_store(Some(store.clone()));
    assert_eq!(login_remembering_at(&base, USER, PASSWORD, None, false).unwrap().1, Some(NOT_FORGOTTEN));
    let _ = asked.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!is_remembered(), "an intervening status doesn't make it usable again");
    // The session ends: the earlier account isn't signed in with.
    logout();
    assert_eq!(restore(), Restored::Nothing);
    assert_eq!(authorization_fresh(DATA), None);
    assert!(asked.recv_timeout(Duration::from_millis(200)).is_err(), "the earlier password is never sent");
}

/// A remembered sign-in the store couldn't delete after it was refused, found damaged or signed
/// out of is never tried again, so it doesn't count as one a request can use (#2378).
#[test]
fn a_remembered_sign_in_that_is_never_tried_again_is_not_usable() {
    assert!(usable_remembered(Known::Kept { retry_at: 0 }));
    assert!(usable_remembered(Known::Kept { retry_at: 4_000_000_000 }));
    assert!(!usable_remembered(Known::Kept { retry_at: i64::MAX }));
    for k in [Known::Unread, Known::Nothing, Known::Unreadable] {
        assert!(!usable_remembered(k));
    }
}
