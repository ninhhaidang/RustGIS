//! A package names the layers it leaves linked to files outside it (#2350): it holds only their
//! paths, so on another computer it opens without them.

use super::common::*;
use super::{CITIES, COUNTRIES, DEM, Layers, populated};
use rustgis_engine::Session;
use serde_json::json;

/// The notices a package command gave since `mark` that say what it left linked.
fn linked_notices(s: &Session, mark: u64) -> Vec<String> {
    s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("outside the package")).collect()
}

fn package(s: &mut Session, f: &Fixture, command: &str, params: serde_json::Value) -> Vec<String> {
    let mut params = params;
    params["path"] = json!(f.file(&format!("out/{command}.zip")));
    let mark = s.notice_mark();
    s.run(command, &params).unwrap();
    linked_notices(s, mark)
}

#[test]
fn project_package_names_the_layers_and_archives_left_linked() {
    let f = Fixture::new("package-linked");
    let (mut s, _) = populated(&f);
    let said = package(&mut s, &f, "project.package", json!({}));
    assert_eq!(said.len(), 1, "one notice, with the count: {said:?}");
    let text = &said[0];
    // The two streamed layers and the local tile archive stay outside; the raster that fits is
    // embedded, so it isn't named.
    assert!(text.starts_with("Package Project: 3 layer(s) stay linked to files outside the package"), "{text}");
    // Top layer first, then the tile archives.
    assert!(text.ends_with(": Countries, Cities, t"), "{text}");
    assert!(!text.contains("my dem"), "an embedded raster is in the package: {text}");
}

#[test]
fn a_package_of_copied_layers_says_nothing_about_links() {
    let f = Fixture::new("package-copied");
    let mut s = session();
    s.run("project.sample", &serde_json::Value::Null).unwrap();
    assert!(package(&mut s, &f, "project.package", json!({})).is_empty());
    assert!(package(&mut s, &f, "map.package", json!({})).is_empty());
}

#[test]
fn map_packages_name_what_they_leave_linked_and_mobile_only_what_it_keeps() {
    let f = Fixture::new("package-map");
    let (mut s, Layers { cities, .. }) = populated(&f);
    let said = package(&mut s, &f, "map.package", json!({}));
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("Package Map: 3 layer(s) stay linked"), "{}", said[0]);
    // A mobile package drops hidden layers: a hidden linked layer isn't in it, so isn't named.
    s.map_mut().layer_mut(cities).unwrap().visible = false;
    let said = package(&mut s, &f, "map.package", json!({"mobile": true, "tolerance": 0}));
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("Package Map: 2 layer(s) stay linked"), "{}", said[0]);
    assert!(!said[0].contains("Cities"), "{}", said[0]);
}

#[test]
fn a_raster_that_cannot_be_embedded_is_named() {
    let f = Fixture::new("package-raster");
    let (mut s, _) = populated(&f);
    // Its file gone since it was added: the package can't embed it, and keeps only its path.
    std::fs::remove_file(f.file(DEM)).unwrap();
    let said = package(&mut s, &f, "project.package", json!({}));
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("Package Project: 4 layer(s) stay linked"), "{}", said[0]);
    assert!(said[0].contains("my dem"), "{}", said[0]);
}

#[test]
fn past_five_names_the_rest_are_counted_over_every_map() {
    let f = Fixture::new("package-many");
    sample_file(&f.file(CITIES), "Cities");
    sample_file(&f.file(COUNTRIES), "Countries");
    let mut s = session();
    for _ in 0..3 {
        add_stream(&mut s, &f.file(CITIES), None);
    }
    s.run("map.new", &json!({"name": "Second"})).unwrap();
    for _ in 0..4 {
        add_stream(&mut s, &f.file(COUNTRIES), None);
    }
    let said = package(&mut s, &f, "project.package", json!({}));
    assert_eq!(said.len(), 1, "{said:?}");
    let text = &said[0];
    assert!(text.starts_with("Package Project: 7 layer(s) stay linked"), "{text}");
    assert!(text.ends_with(" and 2 more"), "{text}");
    let listed = text.rsplit_once(": ").unwrap().1.trim_end_matches(" and 2 more");
    assert_eq!(listed.split(", ").count(), 5, "{text}");
}

#[test]
fn the_notice_waits_for_the_package_to_be_delivered() {
    let f = Fixture::new("package-delivered");
    let (mut s, _) = populated(&f);
    // The desktop app builds the package before its Save dialog, and the browser before its
    // download: a package cancelled there was never made, so building it says nothing yet.
    for kind in ["projectPackage", "mapPackage", "mobilePackage"] {
        let mark = s.notice_mark();
        let (_, _, notes) = rustgis_engine::cmd::items::item_output(&mut s, kind, &json!({"tolerance": 0})).unwrap();
        assert!(linked_notices(&s, mark).is_empty(), "{kind}: said before delivery");
        assert_eq!(notes.texts().filter(|t| t.contains("outside the package")).count(), 1, "{kind}: left for the caller to say");
    }
}

#[test]
fn offline_areas_in_a_project_package_are_counted_too() {
    let f = Fixture::new("package-offline");
    let (mut s, _) = populated(&f);
    // An offline area keeps a copy of the map's layers, a streamed one with its file's path, and
    // its tile archives: the package holds them as paths too.
    s.run("offline.download", &json!({"name": "Field", "extent": [-180.0, -85.0, 180.0, 85.0]})).unwrap();
    let said = package(&mut s, &f, "project.package", json!({}));
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("Package Project: 6 layer(s) stay linked"), "{}", said[0]);
}
