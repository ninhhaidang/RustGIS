//! Token generation for saved ArcGIS connections: the pending operations of a session.
//!
//! `connection.generateToken` sends the connection's user name and a password to the ArcGIS
//! Server token endpoint of the connection's own origin (see
//! [`rustgis_tiles::arcgis_token`](crate::render::rustgis_tiles::arcgis_token)) off the caller's
//! thread, and `connection.tokenStatus` lands the answer: nothing is activated or saved from the
//! network callback. Each operation owns
//!
//! - an immutable snapshot of the connection definition it was started for,
//! - the continuation of the invocation that started it: the scope, account and project it was
//!   admitted under, and its admitted node *without its `password`* (see
//!   [`Session::token_continuation`]). Exactly that caller, root or scoped, can poll or cancel
//!   it; any other is refused before it can touch the result,
//! - a private result slot only its own callback fills, holding a validated token or a fixed
//!   sentence (never the password, never server text), and
//! - a cancellation flag.
//!
//! The password lives in the one POST's form and nowhere else past the invocation that gave it:
//! nothing the operation retains holds it. Cancelling, a failure, a changed definition, a
//! replaced project or revoked authority discards the answer without touching the login the
//! connection had.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::Session;
use crate::connections::{Connection, login_origin, same_login};
use crate::policy::Continuation;
use crate::render::rustgis_tiles::{arcgis_token, cache, logins};
use crate::service_logins as web;

/// A generation already under way for this connection.
pub const ALREADY: &str = "A token is already being generated for this connection. Cancel it first.";
/// A connection with no user to generate a token for.
pub const NO_USER: &str = "This connection has no user: set user (connection.edit) to generate a token.";
/// A password that can't be sent.
pub const BAD_PASSWORD: &str = "The password must be at most 4096 bytes, without control characters.";
/// A generation whose connection changed while it was under way.
pub const CHANGED: &str = "The connection changed while its token was being generated. Generate it again.";

type Slot = Arc<Mutex<Option<Result<arcgis_token::Credential, String>>>>;

/// One generation under way. Deliberately neither `Debug` nor `Clone`.
pub(crate) struct Pending {
    connection: Connection,
    remember: bool,
    owner: Continuation,
    slot: Slot,
    cancelled: Arc<AtomicBool>,
}

impl Pending {
    /// Stops the request at its next look and drops whatever answer is or will be in its slot.
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// What the operation retains of its starting invocation (tests only).
    #[cfg(test)]
    pub(crate) fn owner(&self) -> &Continuation {
        &self.owner
    }
}

/// What a landed generation hands the command that polled it.
pub(crate) struct Ready {
    pub credential: arcgis_token::Credential,
    pub remember: bool,
    /// The invocation that started it, to land the token under.
    pub owner: Continuation,
}

#[expect(clippy::large_enum_variant, reason = "Keep the owned continuation inline instead of allocating again on completion.")]
pub(crate) enum Polled {
    /// Nothing is under way for this connection.
    Idle,
    Pending,
    Ready(Ready),
}

/// Starts generating a token for the saved connection `c`. Nothing is sent when anything about
/// the request is wrong. The endpoint must be an https:// address at the connection's own origin.
pub(crate) fn start(s: &mut Session, c: &Connection, endpoint: &str, password: &str, remember: bool) -> Result<(), String> {
    // A generation left from a replaced project (its connection ids are reused) is not this one's.
    if s.web_logins.pending.get(&c.id).is_some_and(|op| s.check_continuation(&op.owner).is_err()) {
        cancel_for(s, &c.id);
    }
    if s.web_logins.pending.contains_key(&c.id) {
        return Err(ALREADY.into());
    }
    let origin = login_origin(c).ok_or(web::NO_ORIGIN)?;
    arcgis_token::check_endpoint(endpoint, &origin)?;
    if c.user.trim().is_empty() {
        return Err(NO_USER.into());
    }
    if !logins::valid_value(password) {
        return Err(BAD_PASSWORD.into());
    }
    let owner = s.token_continuation(&c.id)?;
    owner.check()?;
    let slot: Slot = Slot::default();
    let cancelled = Arc::new(AtomicBool::new(false));
    let still: cache::Still = {
        let (owner, flag) = (owner.clone(), cancelled.clone());
        Arc::new(move || !flag.load(Ordering::SeqCst) && owner.check().is_ok())
    };
    let done: arcgis_token::Done = {
        let (fill, flag) = (slot.clone(), cancelled.clone());
        Box::new(move |result| {
            if !flag.load(Ordering::SeqCst) {
                *fill.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            }
        })
    };
    s.web_logins.pending.insert(c.id.clone(), Pending { connection: c.clone(), remember, owner, slot, cancelled });
    arcgis_token::generate(endpoint.to_string(), arcgis_token::form(&c.user, password), still, done);
    Ok(())
}

/// Looks at the generation under way for `id`. Another caller's is refused and left as it is; one
/// whose project was replaced or whose authority was revoked is dropped; a finished one is
/// consumed here, whether it gave a token or failed. A token comes out only for the connection
/// definition it was started for.
pub(crate) fn poll(s: &mut Session, current: &Connection) -> Result<Polled, String> {
    let Some(op) = s.web_logins.pending.get(&current.id) else { return Ok(Polled::Idle) };
    s.same_owner(&op.owner)?;
    let owner = op.owner.clone();
    if let Err(why) = s.check_continuation(&owner) {
        cancel_for(s, &current.id);
        return Err(why);
    }
    let landed = op.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(result) = landed else { return Ok(Polled::Pending) };
    let Some(op) = s.web_logins.pending.remove(&current.id) else { return Ok(Polled::Idle) };
    let credential = result?;
    if !same_login(&op.connection, current) {
        return Err(CHANGED.into());
    }
    Ok(Polled::Ready(Ready { credential, remember: op.remember, owner }))
}

/// Cancels the generation of `id` for its owner: whether there was one. Another caller's is
/// refused and left as it is.
pub(crate) fn cancel(s: &mut Session, id: &str) -> Result<bool, String> {
    let Some(op) = s.web_logins.pending.get(id) else { return Ok(false) };
    s.same_owner(&op.owner)?;
    Ok(cancel_for(s, id))
}

/// Cancels the generation of `id` whoever started it: the connection is going away, changing its
/// login or being logged in again. The login it has stays as it is.
pub(crate) fn cancel_for(s: &mut Session, id: &str) -> bool {
    s.web_logins.pending.remove(id).map(|op| op.cancel()).is_some()
}
