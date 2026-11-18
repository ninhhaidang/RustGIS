//! What sessions say of transformers that can't be built again when datum grids arrive, and of
//! download news they ask for too late to hear (#2032 A4). Its own test binary: both are
//! process-wide.

use rustgis_data::raster::crs;
use rustgis_engine::grids::{self, News};
use rustgis_engine::{Project, Session};

/// A kept transformer that can't be built again with a grid that arrived is said by every
/// session, once each, in one notice, and nothing is said without one.
#[test]
fn a_transformer_not_built_again_is_said_by_each_session_once() {
    let said = |s: &Session| {
        s.notices.iter().filter(|n| n.text.contains("couldn't be built again")).map(|n| (n.text.clone(), n.error)).collect::<Vec<_>>()
    };
    let mut a = Session::new(Project::default());
    let _ = a.run("edit.undo", &serde_json::Value::Null);
    assert!(said(&a).is_empty(), "none without one");
    crs::note_rebuild_failure("EPSG 4267");
    let _ = a.run("edit.undo", &serde_json::Value::Null);
    let _ = a.run("edit.undo", &serde_json::Value::Null);
    let heard = said(&a);
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(
        !heard[0].1 && heard[0].0.starts_with("1 coordinate system couldn't be built again") && heard[0].0.contains("EPSG:4267"),
        "{heard:?}"
    );
    // A session started after it hears it too: the transformer it would use is the same.
    let mut b = Session::new(Project::default());
    let _ = b.run("edit.undo", &serde_json::Value::Null);
    assert_eq!(said(&b).len(), 1, "a session started later hears it too");
    // Several at once: one notice, a few named, the rest counted.
    for code in 1..=5 {
        crs::note_rebuild_failure(&format!("EPSG {code}"));
    }
    let _ = a.run("edit.undo", &serde_json::Value::Null);
    let heard = said(&a);
    assert!(heard.len() == 2 && heard[1].0.starts_with("5 coordinate systems") && heard[1].0.contains("and 2 more"), "{heard:?}");
}

/// A session that asks after more download news than is kept was dropped is told how many it
/// missed, once, as information; one that heard everything isn't.
#[test]
fn missed_download_news_is_said_once() {
    let said =
        |s: &Session| s.notices.iter().filter(|n| n.text.contains("came before")).map(|n| (n.text.clone(), n.error)).collect::<Vec<_>>();
    let mut early = Session::new(Project::default());
    let mut keeping_up = Session::new(Project::default());
    for i in 0..300 {
        grids::tell(News::Got { name: format!("g{i}.tif"), bytes: 1 });
        if i % 100 == 0 {
            keeping_up.notice_landed_rasters();
        }
    }
    keeping_up.notice_landed_rasters();
    assert!(said(&keeping_up).is_empty(), "{:?}", said(&keeping_up));
    early.notice_landed_rasters();
    early.notice_landed_rasters();
    let heard = said(&early);
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!(!heard[0].1 && heard[0].0.starts_with("44 notices about datum grid downloads came before they could be said"), "{heard:?}");
}
