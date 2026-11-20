//! A conversion between two systems whose direct operation needs a datum grid RustGIS doesn't have
//! is said as a system's is: by both systems' names, pointing to the setting that downloads it;
//! with downloads on, a read waits for its grid, and turning them on asks for it. Its own test binary: grid folders and
//! downloads are process-wide.

use rustgis_data::raster::{Crs, crs};
use rustgis_engine::grids;
use rustgis_engine::{Project, Session};

/// Runs a command, after which the session says the grid notes of what was converted (unlike the
/// frame path, which looks at most once a second).
fn after_a_command(s: &mut Session) {
    s.run("info.project", &serde_json::Value::Null).unwrap();
}

/// The grids asked for of the server.
static ASKED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// PROJ's grid server, which has nothing here: the test looks at what is asked for, not at what
/// arrives.
fn server(url: &str, _max: usize, _timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    ASKED.lock().unwrap().push(url.to_string());
    Err("HTTP 404".to_string())
}

/// LV03 to LV95's best transformation is by swisstopo's CHENyx06 grid, which neither system's own
/// conversion to WGS 84 needs; without it the next is used, and the session says so once, after
/// the command that converted. A conversion on one datum says nothing. With downloads on, a read
/// converting between the two waits for the pair's grid before it converts, as for a system's;
/// and turning downloads on after the note asks for the pair's grid, though its note was said.
#[test]
fn a_pairs_missing_grid_is_said_once_and_downloaded_when_turned_on() {
    let root = std::env::temp_dir().join(format!("rustgis-pair-grids-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // Only a folder of the test's own: none from this computer (PROJ's).
    crs::set_grid_folders(vec![root.join("datum-grids")]);
    grids::set_download(server);
    let mut last = None;
    for offers in [true, false] {
        let mut s = Session::new(Project::default());
        s.offers_grid_downloads = offers;
        // What a command converts on the session's thread (an export to another system).
        Crs::Epsg(21781).to(Crs::Epsg(2056)).unwrap();
        after_a_command(&mut s);
        let texts: Vec<&String> = s.notices.iter().map(|n| &n.text).collect();
        let notes: Vec<&&String> = texts.iter().filter(|t| t.contains("EPSG:21781 to EPSG:2056")).collect();
        assert_eq!(notes.len(), 1, "{texts:?}");
        assert!(notes[0].contains("needs a grid"), "{texts:?}");
        assert_eq!(notes[0].contains("Settings › Datum Grids › Download Datum Grids"), offers, "{texts:?}");
        // Not again for the same pair.
        Crs::Epsg(21781).to(Crs::Epsg(2056)).unwrap();
        after_a_command(&mut s);
        assert_eq!(s.notices.iter().filter(|n| n.text.contains("EPSG:21781 to EPSG:2056")).count(), 1);
        // A conversion on one datum (UTM 32N to LAEA Europe, both ETRS89) needs no grid.
        Crs::Epsg(25832).to(Crs::Epsg(3035)).unwrap();
        after_a_command(&mut s);
        assert!(!s.notices.iter().any(|n| n.text.contains("EPSG:25832 to EPSG:3035")), "{:?}", s.notices);
        last = Some(s);
    }
    // A read on a thread that may wait: the pair's grid is asked for before the conversion is
    // made (the server's refusal ends the wait).
    grids::set_enabled(true);
    let to = crs::waiting_for_grids(|| Crs::Epsg(21781).to(Crs::Epsg(2056)));
    assert!(to.is_ok());
    assert!(ASKED.lock().unwrap().iter().any(|u| u.contains("CHENyx06")), "{:?}", ASKED.lock().unwrap());
    // Off and on again, which tries the failed ones again: the session asks for the pair's grid,
    // whose note it said with downloads off.
    grids::set_enabled(false);
    let mut s = last.unwrap();
    grids::set_enabled(true);
    let coming = s.want_grids_in_use();
    grids::set_enabled(false);
    let _ = std::fs::remove_dir_all(&root);
    assert!(coming.iter().any(|n| n.contains("CHENyx06")), "{coming:?}");
}
