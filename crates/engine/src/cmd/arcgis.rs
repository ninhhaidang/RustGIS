//! Signing in to ArcGIS Online or an ArcGIS Enterprise portal, so its secured services can be
//! read, and tokens given for an ArcGIS Server. Sessions are kept in memory for the session only,
//! never in the project (see `rustgis_tiles::arcgis_auth`).

use serde_json::{Value, json};

use super::{f, ps};
use crate::render::rustgis_tiles::arcgis_auth::{self, Info};
use crate::{CmdResult, CommandSpec as C, Session};

/// Declares sign-in changes as reserved account operations, independently of their network reach.
pub fn commands() -> Vec<C> {
    vec![
        C::new("arcgis.signIn", "Sign In to ArcGIS", "Home › Add › Service", sign_in).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network).with(crate::policy::Effect::Reserved)).view().params(
            r#"{"portal"?: "the portal's address: an ArcGIS Online organization's (https://myorg.maps.arcgis.com) or an ArcGIS Enterprise portal's (https://gis.example.org/portal); ArcGIS Online when blank", "clientId"?: "the client id of an OAuth app registered on the portal (Developer credentials) whose redirect URLs list http://127.0.0.1:7290/arcgis/callback; RustGIS's own app when absent on ArcGIS Online (required for ArcGIS Enterprise)", "port"?: "the callback server's port (7290; the redirect URL registered must name it)"} → the sign-in page to open in the browser; the session is kept when the browser comes back (arcgis.status tells). Desktop and command line only"#,
        ),
        C::new("arcgis.token", "Use ArcGIS Token", "Home › Add › Service", token).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network).with(crate::policy::Effect::Reserved)).view().params(
            r#"{"url": "an ArcGIS REST address on https:// (a service's, …/rest/services, or a portal's …/sharing/rest)", "token": "an ArcGIS token for it"} — kept for the session only, never in the project; sent to that ArcGIS Server's (or portal's) requests only. A service address given to service.layers or service.add with ?token=… does the same"#,
        ),
        C::new("arcgis.signOut", "Sign Out of ArcGIS", "Home › Add › Service", sign_out).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Network).with(crate::policy::Effect::Reserved))
            .view()
            .params(r#"{"portal"?: "a signed-in portal's address, or a server a token was given for (arcgis.status lists them); every one when absent"}"#),
        C::new("arcgis.cancelSignIn", "Cancel ArcGIS Sign-In", "Home › Add › Service", |_s, _p| {
            #[cfg(not(target_arch = "wasm32"))]
            arcgis_auth::cancel_sign_in();
            Ok(status())
        }).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved))
        .view(),
        C::new("arcgis.status", "ArcGIS Sign-In Status", "Home › Add › Service", |_s, _p| Ok(status())).roles(&[]).effects(crate::policy::Effects::NONE).view(),
    ]
}

fn info_json(i: &Info) -> Value {
    json!({
        "portal": i.portal,
        "username": i.username,
        "expires": i.expires_at.map(crate::render::rustgis_tiles::earthdata::date_of),
        "renews": i.renews,
        "kind": if i.token { "token" } else { "signIn" },
    })
}

/// The sessions kept, and the sign-in under way or how the last one ended.
pub fn status() -> Value {
    let sessions: Vec<Value> = arcgis_auth::list().iter().map(info_json).collect();
    json!({"sessions": sessions, "redirectUri": arcgis_auth::redirect_uri(arcgis_auth::PORT), "signIn": progress()})
}

#[cfg(not(target_arch = "wasm32"))]
fn progress() -> Value {
    use arcgis_auth::Progress;
    match arcgis_auth::progress() {
        Progress::Idle => json!({"state": "idle"}),
        Progress::Pending { portal } => json!({"state": "pending", "portal": portal}),
        Progress::Done(i) => json!({"state": "signedIn", "portal": i.portal, "username": i.username}),
        Progress::Failed(e) => json!({"state": "failed", "error": e}),
    }
}

/// The browser doesn't sign in itself (see `start`).
#[cfg(target_arch = "wasm32")]
fn progress() -> Value {
    json!({"state": "idle"})
}

fn sign_in(_s: &mut Session, p: &Value) -> CmdResult {
    let portal = arcgis_auth::portal_base(ps(p, "portal").unwrap_or(""))?;
    // RustGIS's own app on ArcGIS Online when none is given; an Enterprise portal registers one.
    let client_id = arcgis_auth::client_id_for(&portal, ps(p, "clientId").unwrap_or("")).ok_or(
        "clientId is required for an ArcGIS Enterprise portal: the client id of an OAuth app registered on it (Content › New item › Developer credentials)",
    )?;
    let port = match f(p, "port") {
        Some(n) if (1024.0..=65535.0).contains(&n) && n.fract() == 0.0 => n as u16,
        Some(_) => return Err("port is a whole number from 1024 to 65535".into()),
        None => arcgis_auth::PORT,
    };
    start(&portal, &client_id, port)
}

#[cfg(not(target_arch = "wasm32"))]
fn start(portal: &str, client_id: &str, port: u16) -> CmdResult {
    let url = arcgis_auth::start_sign_in(portal, client_id, port)?;
    Ok(json!({"url": url, "portal": portal, "redirectUri": arcgis_auth::redirect_uri(port), "state": "pending"}))
}

#[cfg(target_arch = "wasm32")]
fn start(_portal: &str, _client_id: &str, _port: u16) -> CmdResult {
    Err("signing in to ArcGIS needs the desktop app; in the browser, give a token (arcgis.token, or the service's address with ?token=…)"
        .into())
}

fn token(_s: &mut Session, p: &Value) -> CmdResult {
    let url = ps(p, "url").map(str::trim).filter(|u| !u.is_empty()).ok_or("url is required (an ArcGIS REST address)")?;
    let token = ps(p, "token").map(str::trim).filter(|t| !t.is_empty()).ok_or("token is required")?;
    let root = arcgis_auth::add_token(url, token)?;
    crate::render::rustgis_tiles::cache::clear();
    Ok(json!({"server": root, "sessions": status()["sessions"]}))
}

fn sign_out(_s: &mut Session, p: &Value) -> CmdResult {
    // Only an absent (or blank) portal means every session: junk is refused, not taken for it.
    if p.get("portal").is_some_and(|v| !v.is_null() && !v.is_string()) {
        return Err("portal is a signed-in portal's address (arcgis.status lists them)".into());
    }
    let portal = match ps(p, "portal").map(str::trim).filter(|s| !s.is_empty()) {
        // A portal's address in any of its forms, or a server root as arcgis.status lists it.
        Some(given) => {
            let base = arcgis_auth::portal_base(given).unwrap_or_else(|_| given.to_string());
            let known = arcgis_auth::list();
            let pick = [base.as_str(), given.trim_end_matches('/')].into_iter().find(|c| known.iter().any(|i| i.portal == *c));
            Some(pick.ok_or_else(|| format!("{given}: not signed in (arcgis.status lists the sessions)"))?.to_string())
        }
        None => None,
    };
    let (revocations, gone) = arcgis_auth::sign_out(portal.as_deref());
    // Tiles and documents read with the token are cached by address: they go with it.
    if gone > 0 {
        crate::render::rustgis_tiles::cache::clear();
    }
    #[cfg(not(target_arch = "wasm32"))]
    arcgis_auth::revoke_all(revocations);
    #[cfg(target_arch = "wasm32")]
    drop(revocations);
    Ok(json!({"signedOut": gone, "sessions": status()["sessions"]}))
}
