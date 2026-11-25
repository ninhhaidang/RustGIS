//! The read routes for one file agree: the same layers and rows, and the same loss codes, on
//! every route.
//!
//! A format can reach RustGIS several ways: by path (`layer.addData`), linked in place
//! (`"stream": true`, then Load All), as bytes (`layer.addDataBytes`, the browser), as text
//! (`layer.addDataText`), as a finished download (`layer.addDownload`) and inside a zip. Each
//! file of the reading-loss corpus (`never_silent_reading.rs`), clean and lossy, is read every
//! way it can travel and compared with the read by path:
//! - the layers' and tables' names, fields, rows and coordinate systems;
//! - the loss codes the notices carry, and whether anything is said at all (wording may differ);
//! - a refusal: a route that refuses what the path reads, or reads what it refuses, differs.
//!
//! A linked layer's codes are the ones linking it says; Load All may repeat them but says no
//! new one.
//!
//! Every file is read by path and linked in place. Files with others beside them (a
//! Shapefile's parts) and folders (FileGDB) aren't read as bytes, text or a download, and folders
//! aren't zipped; text is read only when it is UTF-8. GDAL and DuckDB are not routes
//! here yet.
//!
//! Routes that read a format differently today are listed in [`DIFFERENT`] with how they differ;
//! any other difference still fails, and a route that starts agreeing fails until its entry is
//! removed, so the list only shrinks.
use super::never_silent_reading::cases;
use base64::Engine;
use rustgis_engine::data::loss::LossCode;
use rustgis_engine::{Project, Session};
use serde_json::json;
use std::collections::HashSet;

/// The ways a file can travel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Route {
    Path,
    Stream,
    Bytes,
    Text,
    Download,
    Zip,
}

/// How a route listed in [`DIFFERENT`] reads a file differently from its path; anything else
/// it does differently still fails.
#[derive(Clone, Copy)]
enum Differs {
    /// Refuses it, saying why.
    Refuses,
    /// Adds only the first of its layers, says so, and says no code the path doesn't.
    FirstLayer,
}

/// Routes that read a corpus file differently from its path today: the file, the route, and
/// how. Each is said, not silent.
const DIFFERENT: &[(&str, Route, Differs)] = &[
    // A zip's feature-service page, workbook, GeoRSS feed and LandXML file aren't read.
    ("a.feature.pbf", Route::Zip, Differs::Refuses),
    ("a.xlsx", Route::Zip, Differs::Refuses),
    ("a.xls", Route::Zip, Differs::Refuses),
    ("a.xlsb", Route::Zip, Differs::Refuses),
    ("a.ods", Route::Zip, Differs::Refuses),
    ("a.rss", Route::Zip, Differs::Refuses),
    ("a.xml", Route::Zip, Differs::Refuses),
    // GeoRSS and LandXML aren't read from text.
    ("a.rss", Route::Text, Differs::Refuses),
    ("a.xml", Route::Text, Differs::Refuses),
    // A KML document or GPX file of several layers adds them all by path, bytes or download.
    ("a.kml", Route::Text, Differs::FirstLayer),
    ("a.kml", Route::Zip, Differs::FirstLayer),
    ("a.kmz", Route::Zip, Differs::FirstLayer),
    ("a.gpx", Route::Text, Differs::FirstLayer),
    ("a.gpx", Route::Zip, Differs::FirstLayer),
];

/// What one read gave: each layer and table as text, the loss codes said, and whether
/// anything was said.
#[derive(Debug, PartialEq)]
struct Read {
    layers: Vec<String>,
    codes: HashSet<LossCode>,
    said: bool,
}

/// Reads `content` named `file` (with `beside` next to it) by `route` in a fresh session, in
/// `dir`; `None` when the file can't travel that way.
fn read(dir: &std::path::Path, route: Route, file: &str, content: &[u8], beside: &[(&str, Vec<u8>)]) -> Option<Result<Read, String>> {
    let alone = beside.is_empty() && !file.ends_with(".gdb");
    let travels = match route {
        Route::Path | Route::Stream => true,
        Route::Bytes | Route::Download => alone,
        Route::Text => alone && std::str::from_utf8(content).is_ok(),
        Route::Zip => !file.ends_with(".gdb") && !file.ends_with(".zip"),
    };
    if !travels {
        return None;
    }
    let _ = std::fs::remove_dir_all(dir);
    // Preparing the files can't fail quietly: that would skip the comparison.
    std::fs::create_dir_all(dir).unwrap_or_else(|e| unprepared(file, route, &e));
    let path = dir.join(file);
    if file.ends_with(".gdb") {
        // The corpus stores a FileGDB folder zipped; RustGIS opens the folder.
        std::fs::create_dir_all(&path).unwrap_or_else(|e| unprepared(file, route, &e));
        for (name, bytes) in rustgis_engine::data::zip::read(content).unwrap_or_else(|e| unprepared(file, route, &e)) {
            std::fs::write(path.join(name), bytes).unwrap_or_else(|e| unprepared(file, route, &e));
        }
    } else {
        std::fs::write(&path, content).unwrap_or_else(|e| unprepared(file, route, &e));
    }
    for (name, bytes) in beside {
        std::fs::write(dir.join(name), bytes).unwrap_or_else(|e| unprepared(file, route, &e));
    }
    let mut s = Session::new(Project::default());
    let mark = s.notice_mark();
    let ran = match route {
        Route::Path => s.run("layer.addData", &json!({"path": path})),
        Route::Stream => s.run("layer.addData", &json!({"path": path, "stream": true})),
        Route::Bytes => {
            s.run("layer.addDataBytes", &json!({"fileName": file, "data": base64::engine::general_purpose::STANDARD.encode(content)}))
        }
        Route::Text => s.run(
            "layer.addDataText",
            &json!({"fileName": file, "text": std::str::from_utf8(content).unwrap_or_else(|e| unprepared(file, route, &e))}),
        ),
        Route::Download => {
            let id = s.download_started();
            s.download_landed(id, content.to_vec());
            s.run("layer.addDownload", &json!({"id": id, "url": format!("https://example.org/{file}")}))
        }
        Route::Zip => {
            let mut entries = vec![(file.to_string(), content.to_vec())];
            entries.extend(beside.iter().map(|(n, b)| (n.to_string(), b.clone())));
            let zip = dir.join("routes.zip");
            let bytes = rustgis_engine::data::zip::write(&entries).unwrap_or_else(|e| unprepared(file, route, &e));
            std::fs::write(&zip, bytes).unwrap_or_else(|e| unprepared(file, route, &e));
            s.run("layer.addData", &json!({"path": zip}))
        }
    };
    if let Err(e) = ran {
        return Some(Err(e));
    }
    let notices = s.notices_since(mark);
    // Linking in place says so; that is the route, not the file.
    let notices: Vec<_> = notices.iter().filter(|n| route != Route::Stream || !n.text.contains("read in place a view at a time")).collect();
    let codes: HashSet<LossCode> = notices.iter().filter_map(|n| n.loss()).collect();
    let said = !notices.is_empty();
    // A linked layer's rows come with Load All, which says no loss linking didn't.
    let linked: Vec<u32> = s.map().layers.iter().filter(|l| l.is_streamed()).map(|l| l.id).collect();
    for id in linked {
        let mark = s.notice_mark();
        if let Err(e) = s.run("layer.loadAll", &json!({"layer": id})) {
            return Some(Err(format!("Load All: {e}")));
        }
        let more: Vec<_> = s.notices_since(mark).iter().filter_map(|n| n.loss()).filter(|c| !codes.contains(c)).collect();
        if !more.is_empty() {
            return Some(Err(format!("Load All said {more:?}, which linking didn't")));
        }
    }
    let mut layers: Vec<String> = s
        .map()
        .layers
        .iter()
        .map(|l| format!("{} {:?} {:?} {:?}", l.dataset.name, l.dataset.fields, l.dataset.features, l.dataset.held_in))
        .collect();
    layers.extend(s.project.tables.iter().map(|t| format!("table {} {:?} {:?}", t.dataset.name, t.dataset.fields, t.dataset.features)));
    Some(Ok(Read { layers, codes, said }))
}

/// Fails the test on a file that couldn't be prepared for `route`.
fn unprepared(file: &str, route: Route, e: &dyn std::fmt::Display) -> ! {
    panic!("preparing {file} for {route:?}: {e}")
}

/// Whether a refusal is a crash `Session::run` caught, which is a bug on any route.
fn crashed(read: &Result<Read, String>) -> bool {
    read.as_ref().err().is_some_and(|e| e.contains("failed unexpectedly") || e.contains("stopped unexpectedly"))
}

/// What differs between the path's read and another route's, or `None` when they agree.
fn difference(path: &Result<Read, String>, other: &Result<Read, String>) -> Option<String> {
    match (path, other) {
        _ if crashed(path) || crashed(other) => Some(format!("crashed: by path {path:?}; here {other:?}")),
        (Err(_), Err(_)) => None,
        (Ok(_), Err(e)) => Some(format!("refused what the path reads: {e}")),
        (Err(e), Ok(_)) => Some(format!("read what the path refuses ({e})")),
        (Ok(a), Ok(b)) if a == b => None,
        (Ok(a), Ok(b)) => {
            let only = |p: &[String], q: &[String]| p.iter().filter(|l| !q.contains(l)).cloned().collect::<Vec<_>>();
            let mut out = Vec::new();
            if a.layers != b.layers {
                out.push(format!("layers by path only {:?}; here only {:?}", only(&a.layers, &b.layers), only(&b.layers, &a.layers)));
            }
            if a.codes != b.codes {
                out.push(format!("codes by path {:?}; here {:?}", a.codes, b.codes));
            }
            if a.said != b.said {
                let says = |said: bool| if said { "says something" } else { "says nothing" };
                out.push(format!("the path {}; this route {}", says(a.said), says(b.said)));
            }
            Some(out.join("; "))
        }
    }
}

/// Whether `other` differs from the path's read of `content` (named `file`) only as `how` says.
fn differs_as(how: Differs, file: &str, content: &[u8], path: &Result<Read, String>, other: &Result<Read, String>) -> bool {
    match (how, path, other) {
        // A crash `Session::run` caught is a bug, never the refusal listed.
        (Differs::Refuses, Ok(_), Err(_)) => !crashed(other),
        (Differs::FirstLayer, Ok(a), Ok(b)) => {
            // The first in the file's own order: the map lists the path's layers in the order
            // they were added, not read.
            let first =
                rustgis_engine::data::io::read_layers(file, content).ok().and_then(|l| l.into_iter().next()).map(|l| l.dataset.name);
            let is_first = |layer: &String| first.as_ref().is_some_and(|name| layer.starts_with(&format!("{name} ")));
            b.said
                && b.codes.is_subset(&a.codes)
                && a.layers.len() > 1
                && b.layers.len() == 1
                && a.layers.contains(&b.layers[0])
                && is_first(&b.layers[0])
        }
        _ => false,
    }
}

/// Reads every corpus file by `route` and by path, and fails on a difference [`DIFFERENT`]
/// doesn't list, or an entry for `route` that no longer differs.
fn route_reads_as_the_path_does(route: Route) {
    let root = std::env::temp_dir().join(format!("rustgis-read-routes-{route:?}-{}", std::process::id()));
    let dir = root.join("case");
    let mut failures = Vec::new();
    let mut differing = HashSet::new();
    let mut compared = 0;
    for c in cases(&root.join("fixtures")) {
        for (kind, content, beside) in [("clean", &c.clean, &c.beside[0]), ("lossy", &c.lossy, &c.beside[1])] {
            let Some(other) = read(&dir, route, c.file, content, beside) else { continue };
            let Some(by_path) = read(&dir, Route::Path, c.file, content, beside) else { continue };
            compared += 1;
            let Some(what) = difference(&by_path, &other) else { continue };
            let listed = DIFFERENT.iter().find(|&&(file, r, _)| file == c.file && r == route);
            if listed.is_some_and(|&(_, _, how)| differs_as(how, c.file, content, &by_path, &other)) {
                differing.insert(c.file);
            } else {
                failures.push(format!("{} [{}, {kind}]: {what}", c.name, c.file));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    for &(file, r, _) in DIFFERENT {
        if r == route && !differing.contains(file) {
            failures.push(format!("{file} now reads as its path does: remove it from DIFFERENT"));
        }
    }
    assert!(compared > 0, "no file travels by {route:?}");
    assert!(failures.is_empty(), "{} of {compared} reads by {route:?} differ from the path's:\n{}", failures.len(), failures.join("\n"));
}

/// Linked in place (`"stream": true`), then Load All.
#[test]
fn a_linked_file_reads_as_its_path_does() {
    route_reads_as_the_path_does(Route::Stream);
}

/// `layer.addDataBytes`, as the browser reads a file.
#[test]
fn bytes_read_as_their_path_does() {
    route_reads_as_the_path_does(Route::Bytes);
}

/// `layer.addDataText`, for files that are UTF-8.
#[test]
fn text_reads_as_its_path_does() {
    route_reads_as_the_path_does(Route::Text);
}

/// `layer.addDownload` of a download that landed.
#[test]
fn a_download_reads_as_its_path_does() {
    route_reads_as_the_path_does(Route::Download);
}

/// `layer.addData` of a zip holding the file and those beside it.
#[test]
fn a_zipped_file_reads_as_its_path_does() {
    route_reads_as_the_path_does(Route::Zip);
}
