//! Resolves address-only project origins against trusted saved definitions, never project IDs.
use crate::Session;
#[cfg(not(target_arch = "wasm32"))]
use crate::connections::{Connection, LoginReason, LoginRequest, database_key, layer_database};

pub(crate) fn clear_active() {
    #[cfg(not(target_arch = "wasm32"))]
    rustgis_data::source::duckdb::forget_connection_passwords();
}

pub(crate) fn restore_active(s: &mut Session) {
    #[cfg(not(target_arch = "wasm32"))]
    for (key, id) in s.database_logins.activations() {
        if let Some(pw) = s.database_logins.known_password(&id) {
            rustgis_data::source::duckdb::set_connection_password(&key, &id, Some(pw.expose()));
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = s;
}

#[cfg(not(target_arch = "wasm32"))]
fn groups(s: &Session) -> std::collections::BTreeMap<(&'static str, String), Vec<u32>> {
    let mut groups = std::collections::BTreeMap::<_, Vec<_>>::new();
    for l in s.project.maps.iter().flat_map(|m| &m.layers) {
        let Some(src) = l.source.as_ref().filter(|src| !src.is_open()) else { continue };
        let Some((_, db)) = layer_database(src) else { continue };
        let Some(key) = database_key(&crate::sources::redact(db)) else { continue };
        groups.entry(key).or_default().push(l.id);
    }
    groups
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn request_password(s: &mut Session, c: &Connection) {
    if let Some((kind, key)) = database_key(&c.address) {
        let layers = groups(s).remove(&(kind, key.clone())).unwrap_or_default();
        s.database_logins.request(LoginRequest {
            reason: LoginReason::Password,
            kind,
            key,
            connections: vec![c.id.clone()],
            layers,
            tiles: 0,
            opened: s.opened,
            prompted: false,
        });
    }
}

/// Resolves every unopened database of the project, and the web-service logins its layers read
/// with: a project was opened or imported.
pub(crate) fn resolve_project(s: &mut Session) {
    #[cfg(not(target_arch = "wasm32"))]
    resolve(s, None);
    crate::service_logins::resolve(s, None);
}

/// Resolves only the databases `keys` names: a saved definition or a single layer change can only
/// affect those, so an unrelated database that is down is not connected to again on every edit.
pub(crate) fn resolve_keys(s: &mut Session, keys: &[(&'static str, String)]) {
    #[cfg(target_arch = "wasm32")]
    let _ = (s, keys);
    #[cfg(not(target_arch = "wasm32"))]
    resolve(s, Some(keys));
}

#[cfg(not(target_arch = "wasm32"))]
fn resolve(s: &mut Session, only: Option<&[(&'static str, String)]>) {
    for ((kind, key), layers) in groups(s) {
        if let Some(only) = only
            && !only.iter().any(|(k, database)| *k == kind && *database == key)
        {
            continue;
        }
        use rustgis_data::source::duckdb;
        if duckdb::has_explicit_password(&key) {
            s.database_logins.clear_requests_for(kind, &key);
            continue;
        }
        let selected = s
            .database_logins
            .selection(kind, &key)
            .and_then(|id| s.connections.find(id))
            .filter(|c| database_key(&c.address).is_some_and(|k| k == (kind, key.clone())))
            .cloned();
        let matches: Vec<_> = s.connections.matching_key(kind, &key).into_iter().cloned().collect();
        let candidates: Vec<_> = matches.iter().filter(|c| c.remembered && !s.database_logins.is_rejected(&c.id)).cloned().collect();
        let chosen = if let Some(c) = selected {
            Some(c)
        } else if candidates.len() == 1 {
            candidates.first().cloned()
        } else {
            duckdb::set_connection_password(&key, "", None);
            s.database_logins.drop_activation(&key);
            if candidates.len() > 1 {
                let ids = candidates.iter().map(|c| c.id.clone()).collect();
                s.database_logins.request(LoginRequest {
                    reason: LoginReason::Choose,
                    kind,
                    key: key.clone(),
                    connections: ids,
                    layers: layers.clone(),
                    tiles: 0,
                    opened: s.opened,
                    prompted: false,
                });
            } else if let Some(c) = matches.iter().find(|c| s.database_logins.is_rejected(&c.id)) {
                request_password(s, c);
            }
            None
        };
        let Some(c) = chosen else { continue };
        match super::credential(s, &c) {
            Ok(Some((pw, saved))) => {
                super::activate(s, &c, pw.expose());
                super::reopen(s, &c, saved, !saved, true);
            }
            Ok(None) => {
                duckdb::set_connection_password(&key, &c.id, None);
                s.database_logins.drop_activation(&key);
                request_password(s, &c);
            }
            Err(e) => {
                duckdb::set_connection_password(&key, &c.id, None);
                s.database_logins.drop_activation(&key);
                s.notify(e, true);
                request_password(s, &c);
            }
        }
    }
}
