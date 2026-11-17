//! `connection.generateToken`, `connection.tokenStatus` and `connection.cancelToken`: making an
//! ArcGIS token for a saved connection from its user and a password, in the background.
//!
//! All three are session operations: they change no project, make no undo step, and take no
//! response, token or completion from their caller. The work, its ownership and its landing are
//! [`crate::service_tokens`]'s.

use serde_json::{Value, json};

use super::{Reopened, activate_token, finished_login, found, remember_flag};
use crate::service_logins as web;
use crate::service_tokens::{self as tokens, Polled, Ready};
use crate::{CmdResult, Session};

/// A required text parameter, as it was given.
fn text<'a>(p: &'a Value, key: &str) -> Result<&'a str, String> {
    match p.get(key) {
        Some(Value::String(t)) if !t.is_empty() => Ok(t),
        None | Some(Value::Null | Value::String(_)) => Err(format!("{key} is required")),
        Some(_) => Err(format!("{key} is text")),
    }
}

pub(super) fn generate(s: &mut Session, p: &Value) -> CmdResult {
    let c = found(s, p)?.clone();
    if !web::is_service(&c) {
        return Err(format!("{} connections don't log in with a token: only web-service connections do", c.kind));
    }
    if c.auth != "arcgis" {
        return Err("this connection doesn't log in with an ArcGIS token: set auth to arcgis (connection.edit) first".into());
    }
    let endpoint = text(p, "tokenEndpoint")?.trim();
    let password = text(p, "password")?;
    let remember = remember_flag(p)?;
    tokens::start(s, &c, endpoint, password, remember)?;
    Ok(json!({"id": c.id, "pending": true}))
}

pub(super) fn status(s: &mut Session, p: &Value) -> CmdResult {
    let c = found(s, p)?.clone();
    match tokens::poll(s, &c)? {
        Polled::Idle => Ok(json!({"id": c.id, "pending": false, "loggedIn": web::logged_in(s, &c)})),
        Polled::Pending => Ok(json!({"id": c.id, "pending": true})),
        Polled::Ready(Ready { credential, remember, owner }) => {
            // Logging in is the user's answer to a request waiting at this origin (the login ends it).
            let answers = web::answers(s, &c);
            // Landed under the authority that started it, never the poller's own.
            let landed: Result<Reopened, String> = s.resume(&owner, |s| activate_token(s, &c, &credential, false, remember))?;
            let mut out = finished_login(s, c, answers, landed?)?;
            out["pending"] = json!(false);
            Ok(out)
        }
    }
}

pub(super) fn cancel(s: &mut Session, p: &Value) -> CmdResult {
    let c = found(s, p)?.clone();
    Ok(json!({"id": c.id, "cancelled": tokens::cancel(s, &c.id)?}))
}
