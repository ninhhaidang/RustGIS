//! The Copernicus Data Space Ecosystem's sign-in, so the files on its data hosts (the Copernicus
//! Land Monitoring Service's COGs, the Sentinel mosaics) can be read by window with `layer.addUrl`
//! (see `rustgis_tiles::copernicus`). The password is sent once, to CDSE's sign-in host, and
//! never kept; the tokens live in memory for the session only, never in the project.

use serde_json::{Value, json};

use crate::render::rustgis_tiles::copernicus;
use crate::{CmdResult, CommandSpec as C, Session};

/// Declares sign-in changes as reserved account operations: no plugin or agent signs in, and the
/// assistant needs the user's approval.
pub fn commands() -> Vec<C> {
    vec![
        C::new("copernicus.login", "Sign In to Copernicus Data Space", "Home › Add › Web Data", login).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network).with(crate::policy::Effect::Reserved)).view().params(
            r#"{"username": "a Copernicus Data Space account's username (its email address; free at https://dataspace.copernicus.eu)", "password": "its password: sent once, to CDSE's sign-in host only (identity.dataspace.copernicus.eu), never kept", "totp"?: "the one-time code, for an account with two-factor authentication"} — the token is kept for the session only, never in the project, renewed before it expires, and sent only to CDSE's data hosts (https://download.dataspace.copernicus.eu, zipper.dataspace.copernicus.eu) when their files are read (layer.addUrl with a STAC asset's alternate https address). Desktop and command line only"#,
        ),
        C::new("copernicus.logout", "Sign Out of Copernicus Data Space", "Home › Add › Web Data", logout).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved))
            .view()
            .params("{} — forgets the sign-in's tokens and empties the download cache (what was read with them)"),
        C::new("copernicus.status", "Copernicus Data Space Status", "Home › Add › Web Data", |_s, _p| Ok(status())).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params("{} — whether a sign-in is kept, as whom, when its token expires and until when it is renewed, and whether one expired (sign in again with copernicus.login)"),
    ]
}

/// The sign-in kept, never its tokens.
fn status() -> Value {
    let date = crate::render::rustgis_tiles::earthdata::date_of;
    match copernicus::signed_in() {
        Some(i) => {
            json!({"signedIn": true, "username": i.username, "expires": date(i.expires_at), "renewsUntil": i.renews_until.map(date), "expired": false})
        }
        None => json!({"signedIn": false, "expired": copernicus::expired()}),
    }
}

/// Runs on the UI thread: it is one request (a 30 s timeout), not work that grows with its input,
/// and nothing can be read with the sign-in until it answers.
#[cfg(not(target_arch = "wasm32"))]
fn login(_s: &mut Session, p: &Value) -> CmdResult {
    // Text only: a number would lose a one-time code's leading zeros. Checked before anything is
    // sent, and never quoted back; a missing one is blank (and refused if it is required).
    for k in ["username", "password", "totp"] {
        if p.get(k).is_some_and(|v| !v.is_string() && !v.is_null()) {
            return Err(format!("{k} is text, in quotes (\"{k}\": \"…\")"));
        }
    }
    let text = |k: &str| super::ps(p, k).unwrap_or("");
    copernicus::login(text("username"), text("password"), super::ps(p, "totp"))?;
    // Files refused before the sign-in are asked for again.
    crate::render::rustgis_tiles::cache::clear();
    Ok(status())
}

/// Not in the browser yet: the web app's requests don't carry the token.
#[cfg(target_arch = "wasm32")]
fn login(_s: &mut Session, _p: &Value) -> CmdResult {
    Err("signing in to Copernicus Data Space is for the desktop app and the command line".into())
}

fn logout(_s: &mut Session, _p: &Value) -> CmdResult {
    copernicus::logout();
    // Files read with the token are cached by address: they go with it, as at an Earthdata or
    // ArcGIS sign-out.
    crate::render::rustgis_tiles::cache::sign_out();
    Ok(status())
}
