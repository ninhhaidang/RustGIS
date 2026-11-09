//! Settings › Performance › Threads (#1562): the most threads one piece of work uses, kept as a
//! preference and set at start. The count is the engine's shared one (`rustgis_data::threads`),
//! which the places that start threads for CPU work take, as `gp.threads` sets it.

use rustgis_engine::data::threads;
use serde_json::{Value, json};

use crate::RustGisApp;

/// The preference the setting is kept under: a whole number, or `auto` for the default.
pub(crate) const PREF: &str = "max_threads";
/// What the preference holds for the default.
const AUTO: &str = "auto";

/// Reads the preference at start (from [`RustGisApp::with_services`]). A value that isn't a
/// setting `threads` takes (edited by hand) means the default, and says so.
pub(crate) fn load(app: &mut RustGisApp) {
    let Some(saved) = app.services.load_pref.as_ref().and_then(|load| load(PREF)) else { return };
    let saved = saved.trim();
    if saved == AUTO {
        return;
    }
    let set = saved.parse::<usize>().map_err(|_| String::new()).and_then(|n| threads::set_max(Some(n)));
    if set.is_err() {
        // The default, as the notice says, whatever an earlier start in this process set.
        let _ = threads::set_max(None);
        let text = crate::i18n::fmt(
            "The saved thread setting “{value}” isn't a number from 1 to 1024; RustGIS uses the default.",
            &[("value", saved)],
        );
        app.session.notify(text, true);
    }
}

/// `ui.threads`: sets the most threads one piece of work uses (`threads`, 1 to 1024, or `null` for
/// the default) and keeps it for the next start; says the setting, the threads it gives and the
/// cores. The browser has no threads, so there it can't be set.
pub(crate) fn command(app: &mut RustGisApp, p: &Value) -> Result<Value, String> {
    if let Some(v) = p.get("threads") {
        if cfg!(target_arch = "wasm32") {
            return Err("the browser runs everything on one thread".into());
        }
        let n = threads::parse(v)?;
        threads::set_max(n)?;
        if let Some(save) = &app.services.save_pref {
            save(PREF, &n.map_or(AUTO.to_string(), |n| n.to_string()));
        }
    }
    Ok(json!({"threads": threads::setting(), "max": threads::max(), "default": threads::default_max(), "cores": threads::cores()}))
}

/// The settings Settings › Threads offers on a computer with `cores` cores: powers of two below
/// them, and the cores themselves, up to the largest setting taken ([`threads::MOST`]).
pub(crate) fn choices(cores: usize) -> Vec<usize> {
    let top = cores.clamp(1, threads::MOST);
    let mut out: Vec<usize> = std::iter::successors(Some(1usize), |n| n.checked_mul(2)).take_while(|n| *n < top).collect();
    out.push(top);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choices_go_up_to_the_cores() {
        assert_eq!(choices(1), vec![1]);
        assert_eq!(choices(8), vec![1, 2, 4, 8]);
        assert_eq!(choices(12), vec![1, 2, 4, 8, 12]);
        assert_eq!(choices(0), vec![1]);
        assert_eq!(choices(4096).last(), Some(&threads::MOST), "never past what a setting takes");
    }
}
