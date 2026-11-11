//! The worldview: whose borders the built-in countries and the basemap's world outline show
//! (Settings › Language › Worldview). The default is built in; every other one is downloaded when
//! it is chosen (or, for China, when the system calls for it), off the frame through the tile
//! cache's downloads (a thread natively, fetch on the web), and kept in the cache folder.

use rustgis_engine::sample::{self, View, Worldview};

use crate::{RustGisApp, Slot, i18n};

/// A worldview's name in the interface language.
pub fn name(w: Worldview) -> &'static str {
    match w.name() {
        "Default" => tl!("Default"),
        "Argentina" => tl!("Argentina"),
        "Bangladesh" => tl!("Bangladesh"),
        "Brazil" => tl!("Brazil"),
        "China" => tl!("China"),
        "Egypt" => tl!("Egypt"),
        "France" => tl!("France"),
        "Germany" => tl!("Germany"),
        "Greece" => tl!("Greece"),
        "India" => tl!("India"),
        "Indonesia" => tl!("Indonesia"),
        "Israel" => tl!("Israel"),
        "Italy" => tl!("Italy"),
        "Japan" => tl!("Japan"),
        "Morocco" => tl!("Morocco"),
        "Nepal" => tl!("Nepal"),
        "Netherlands" => tl!("Netherlands"),
        "Pakistan" => tl!("Pakistan"),
        "Palestine" => tl!("Palestine"),
        "Poland" => tl!("Poland"),
        "Portugal" => tl!("Portugal"),
        "Russia" => tl!("Russia"),
        "Saudi Arabia" => tl!("Saudi Arabia"),
        "South Korea" => tl!("South Korea"),
        "Spain" => tl!("Spain"),
        "Sweden" => tl!("Sweden"),
        "Taiwan" => tl!("Taiwan"),
        "Turkey" => tl!("Turkey"),
        "Ukraine" => tl!("Ukraine"),
        "United Kingdom" => tl!("United Kingdom"),
        "United States" => tl!("United States"),
        "Vietnam" => tl!("Vietnam"),
        other => other,
    }
}

/// The download under way: the view and where its outcome lands.
pub struct Fetch {
    pub wanted: View,
    landing: Landing,
}

/// Where a download lands: the bytes, from the services' fetch (checked here), or the outcome of
/// [`sample::fetch`], which checked them on its own thread.
enum Landing {
    Bytes(Slot<Result<Vec<u8>, String>>),
    Done(Slot<Result<(), String>>),
}

/// The worldview preference, with `auto` as the default one in tests (they don't depend on the
/// machine's time zone and locale).
fn pref(app: &RustGisApp) -> &str {
    if cfg!(test) && app.ui.worldview == i18n::AUTO { "default" } else { app.ui.worldview.as_str() }
}

/// The view the preferences ask for.
pub fn wanted(app: &RustGisApp) -> View {
    sample::wanted_view(pref(app), &app.ui.worldview_detail)
}

/// Applies the worldview preferences: shows the view if its countries are in memory, else gets
/// them (the kept copy or a download) and keeps the current borders until they arrive. An
/// untouched sample project's countries follow.
pub fn apply(app: &mut RustGisApp) {
    let before = sample::view();
    let wanted = sample::apply_view(pref(app), &app.ui.worldview_detail);
    let after = sample::view();
    if after != before {
        if !sample::swap_countries(&mut app.session, before, after) && sample::holds_countries(&app.session, before) {
            // The project has changes or is saved: its sample countries stay as they are.
            app.session.notify(
                i18n::fmt(
                    "The basemap now shows the {view} worldview; the project's Countries layer keeps its borders",
                    &[("view", name(after.worldview))],
                ),
                false,
            );
        }
        app.map.invalidate();
    }
    if after == wanted {
        app.ui.worldview_fetch = None;
    } else if app.ui.worldview_fetch.as_ref().is_none_or(|f| f.wanted != wanted) {
        start(app, wanted);
    }
}

/// Starts getting a view's countries.
fn start(app: &mut RustGisApp, wanted: View) {
    let landing = match (&app.services.fetch, wanted.url()) {
        (Some(fetch), Some(url)) => {
            let slot = Slot::default();
            fetch(&url, slot.clone());
            Landing::Bytes(slot)
        }
        _ => {
            let slot: Slot<Result<(), String>> = Slot::default();
            let land = slot.clone();
            sample::fetch(wanted, Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)));
            Landing::Done(slot)
        }
    };
    app.ui.worldview_fetch = Some(Fetch { wanted, landing });
}

/// The worldview being downloaded, if any (the ribbon says so).
pub fn downloading(app: &RustGisApp) -> Option<Worldview> {
    app.ui.worldview_fetch.as_ref().map(|f| f.wanted.worldview)
}

/// Lands a download: shows the view if it is still the one asked for, or says why it can't.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(fetch) = &app.ui.worldview_fetch else { return };
    let wanted = fetch.wanted;
    let landed = match &fetch.landing {
        Landing::Bytes(slot) => slot.lock().unwrap_or_else(|e| e.into_inner()).take().map(|r| r.and_then(|b| sample::accept(wanted, b))),
        Landing::Done(slot) => slot.lock().unwrap_or_else(|e| e.into_inner()).take(),
    };
    let Some(r) = landed else {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    };
    app.ui.worldview_fetch = None;
    match r {
        Ok(()) => apply(app),
        Err(e) => {
            let text = i18n::fmt(
                "Couldn't download the {view} worldview ({error}); the map keeps the {current} borders",
                &[("view", name(wanted.worldview)), ("error", &e), ("current", name(sample::worldview()))],
            );
            app.session.notify(text, true);
        }
    }
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An app whose downloads all end the way `answer` says, at once.
    fn app_fetching(answer: impl Fn(&str) -> Result<Vec<u8>, String> + 'static) -> RustGisApp {
        // Never the computer's own cache folder.
        sample::keep_downloads(false);
        let mut app = RustGisApp::with_sample();
        app.services.fetch = Some(Box::new(move |url: &str, slot: Slot<Result<Vec<u8>, String>>| {
            if let Ok(mut s) = slot.lock() {
                *s = Some(answer(url));
            }
        }));
        app
    }

    /// A worldview is downloaded only when it is chosen, from its own address, and kept as its code.
    /// (The worldview shown is shared by every test in the process, so it isn't looked at here.)
    #[test]
    fn a_chosen_worldview_is_downloaded_then_kept() {
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let log = asked.clone();
        let mut app = app_fetching(move |url| {
            log.borrow_mut().push(url.to_string());
            Ok(sample::COUNTRIES.as_bytes().to_vec())
        });
        let netherlands = Worldview::from_code("nld").unwrap();
        let nld = View::new(netherlands, sample::Detail::Simplified);
        assert!(asked.borrow().is_empty(), "nothing is downloaded before a worldview is chosen");
        assert!(sample::text(nld).is_none());
        app.run("ui.worldview", json!({"worldview": "Netherlands"})).unwrap();
        assert_eq!(app.ui.worldview, "nld");
        assert_eq!(downloading(&app), Some(netherlands));
        assert_eq!(*asked.borrow(), ["https://data.source.coop/opengeos/data/rustgis/worldviews/countries_nld.geojson"]);
        let mark = app.session.notice_mark();
        poll(&mut app, &egui::Context::default());
        assert!(app.session.notices_since(mark).is_empty());
        assert_eq!(downloading(&app), None);
        assert!(sample::text(nld).is_some());
        // Choosing it again downloads nothing.
        app.run("ui.worldview", json!({"worldview": "nld"})).unwrap();
        assert_eq!(asked.borrow().len(), 1);
        assert_eq!(downloading(&app), None);
        // Nor does the built-in default.
        app.run("ui.worldview", json!({"worldview": "default"})).unwrap();
        assert_eq!(asked.borrow().len(), 1);
        // The original detail is a download of its own, kept as a setting.
        app.run("ui.worldview", json!({"worldview": "nld", "detail": "original"})).unwrap();
        assert_eq!(app.ui.worldview_detail, "original");
        assert_eq!(
            asked.borrow().last().map(String::as_str),
            Some("https://data.source.coop/opengeos/data/rustgis/worldviews/original/countries_nld.geojson.gz")
        );
        poll(&mut app, &egui::Context::default());
        assert!(sample::text(View::new(netherlands, sample::Detail::Original)).is_some());
        // Back to simplified: already in memory, nothing downloaded.
        app.run("ui.worldview", json!({"worldview": null, "detail": "simplified"})).unwrap();
        assert_eq!(asked.borrow().len(), 2);
        assert_eq!(app.ui.worldview, "nld");
        assert!(app.run("ui.worldview", json!({"detail": "detailed"})).is_err());
        assert!(app.run("ui.worldview", json!({"worldview": "atlantis"})).is_err());
        assert!(app.run("ui.worldview", json!({"worldview": 7})).is_err());
        assert_eq!((app.ui.worldview.as_str(), app.ui.worldview_detail.as_str()), ("nld", "simplified"));
        // The view shown is the process's: leave it as other tests expect.
        sample::set_view(View::DEFAULT);
    }

    /// A download that fails, or isn't countries, says so and keeps the borders shown.
    #[test]
    fn a_failed_worldview_download_is_told() {
        for (code, answer) in [("pol", Err("HTTP 404".to_string())), ("prt", Ok(b"<html>not found</html>".to_vec()))] {
            let mut app = app_fetching(move |_| answer.clone());
            let w = View::new(Worldview::from_code(code).unwrap(), sample::Detail::Simplified);
            app.run("ui.worldview", json!({"worldview": code})).unwrap();
            let mark = app.session.notice_mark();
            poll(&mut app, &egui::Context::default());
            let told: Vec<_> = app.session.notices_since(mark).iter().map(|n| (n.text.clone(), n.error)).collect();
            assert_eq!(told.len(), 1, "{code}: {told:?}");
            assert!(told[0].1 && told[0].0.contains(w.worldview.name()) && told[0].0.contains("worldview"), "{told:?}");
            // Named once, by the notice: the download's error doesn't repeat it.
            assert_eq!(told[0].0.matches("download").count(), 1, "{told:?}");
            assert!(sample::text(w).is_none());
            assert_eq!(downloading(&app), None);
        }
    }
}
