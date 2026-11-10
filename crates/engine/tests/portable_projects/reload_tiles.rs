use super::common::{Fixture, session};
use rustgis_engine::{
    Session,
    map::TileLayer,
    render::{
        self,
        rustgis_tiles::{cache, mbtiles, pmtiles, source},
    },
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

/// Archive reloads share process-wide caches, and an MBTiles forget clears every resolved style.
/// Serializing these tests and dropping each test's archives keeps Arc-identity assertions about
/// retention rather than about another test's cache work or capacity.
struct Scoped {
    fixture: Fixture,
    _turn: std::sync::MutexGuard<'static, ()>,
}

impl Scoped {
    fn new(label: &str) -> Self {
        static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let turn = TURN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self { fixture: Fixture::new(label), _turn: turn }
    }
}

impl std::ops::Deref for Scoped {
    type Target = Fixture;
    fn deref(&self) -> &Fixture {
        &self.fixture
    }
}

impl Drop for Scoped {
    fn drop(&mut self) {
        for file in std::fs::read_dir(&self.fixture.root).into_iter().flatten().flatten() {
            rustgis_engine::data::mbtiles::close(&file.path());
            cache::reload_archive(&file.path().display().to_string());
        }
    }
}

fn picture(color: [u8; 4]) -> Vec<u8> {
    let mut p = tiny_skia::Pixmap::new(256, 256).unwrap();
    p.fill(tiny_skia::Color::from_rgba8(color[0], color[1], color[2], color[3]));
    p.encode_png().unwrap()
}

fn write_archive(path: &Path, color: [u8; 4], credit: &str) {
    let png = picture(color);
    if path.extension().unwrap() == "pmtiles" {
        let a = pmtiles::NewArchive {
            tile_type: pmtiles::TileType::Png,
            gzipped_tiles: false,
            bounds: [-180.0, -85.0, 180.0, 85.0],
            metadata: json!({"attribution": credit}),
        };
        std::fs::write(path, pmtiles::write(vec![(0, 0, 0, png)], &a).unwrap()).unwrap();
    } else {
        let c = rusqlite::Connection::open(path).unwrap();
        c.execute_batch("CREATE TABLE IF NOT EXISTS metadata (name TEXT, value TEXT); CREATE TABLE IF NOT EXISTS tiles (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_data BLOB); BEGIN IMMEDIATE; DELETE FROM metadata; DELETE FROM tiles;").unwrap();
        c.execute("INSERT INTO metadata VALUES ('format','png'),('minzoom','0'),('maxzoom','0'),('attribution',?1)", [credit]).unwrap();
        c.execute("INSERT INTO tiles VALUES (0,0,0,?1)", [png]).unwrap();
        c.execute_batch("COMMIT").unwrap();
    }
}

fn tile_url(t: &TileLayer) -> String {
    let template = if t.kind == rustgis_engine::map::TileKind::MbTiles { mbtiles::template(&t.url) } else { pmtiles::template(&t.url) };
    template.replace("{z}", "0").replace("{x}", "0").replace("{y}", "0")
}

fn decoded(t: &TileLayer) -> Arc<cache::Payload> {
    let url = tile_url(t);
    for _ in 0..100 {
        match cache::get_tile(&url, cache::Want::Image) {
            cache::State::Ready(p) => return p,
            cache::State::Failed(e) => panic!("{e}"),
            cache::State::Pending => cache::wait(Duration::from_millis(100)),
        }
    }
    panic!("tile did not arrive: {url}");
}

fn style(t: &TileLayer) -> Arc<source::Resolved> {
    for _ in 0..100 {
        if t.kind == rustgis_engine::map::TileKind::MbTiles {
            // MBTiles metadata workers have their own bounded wait, not cache::in_flight.
            let _ = mbtiles::info_ready(&t.url, Duration::from_millis(100)).unwrap();
        }
        match source::resolve(&render::tile_spec(t)) {
            source::Resolve::Ready(r) => return r,
            source::Resolve::Failed(e) => panic!("{e}"),
            source::Resolve::Pending => cache::wait(Duration::from_millis(100)),
        }
    }
    panic!("style did not resolve");
}

fn assert_color(p: &cache::Payload, expected: [u8; 4]) {
    let cache::Payload::Image(p) = p else { panic!("not an image") };
    assert_eq!(p.data().get(..4), Some(expected.as_slice()));
}

fn assert_render(s: &mut Session, expected: [u8; 4], credit: &str) {
    let v = render::View { width: 300.0, height: 200.0, pixels_per_point: 1.0, camera: s.map().camera, sky: false };
    for _ in 0..100 {
        let r = render::render(s.map_mut(), &v).unwrap();
        if !r.tiles_pending {
            assert_eq!(r.pixmap.pixel(150, 100).unwrap().red(), expected[0]);
            assert_eq!(r.pixmap.pixel(150, 100).unwrap().green(), expected[1]);
            assert_eq!(r.pixmap.pixel(150, 100).unwrap().blue(), expected[2]);
            assert!(r.attribution.iter().any(|a| a.contains(credit)), "{:?}", r.attribution);
            return;
        }
        cache::wait(Duration::from_millis(100));
    }
    panic!("render remained pending");
}

#[test]
fn manual_archive_reload_reads_new_pixels_and_credits_without_touching_other_sources() {
    for ext in ["pmtiles", "mbtiles"] {
        let f = Scoped::new(&format!("reload-{ext}"));
        let path = f.file(&format!("linked.{ext}"));
        let other = f.file(&format!("unrelated.{ext}"));
        write_archive(&path, [255, 0, 0, 255], "Old credits");
        write_archive(&other, [0, 0, 255, 255], "Unrelated credits");
        let mut s = session();
        s.run("map.addTiles", &json!({"url": other, "name":"Other", "overlay":true})).unwrap();
        s.run("map.addTiles", &json!({"url": path, "name":"Linked", "overlay":true})).unwrap();
        s.run("map.goToXY", &json!({"lon":10,"lat":20,"scale":50_000_000})).unwrap();
        let i = s.map().tiles.iter().position(|t| t.name == "Linked").unwrap();
        s.map_mut().tiles[i].id = "linked-archive".into();
        s.map_mut().tiles[i].blend = rustgis_engine::map::effects::Blend::Normal;
        s.map_mut().tiles[i].expanded = true;
        let before = s.map().tiles.clone();
        let camera = s.map().camera;
        let other_tile = before.iter().find(|t| t.name == "Other").unwrap();
        let other_pixels = decoded(other_tile);
        let other_style = style(other_tile);
        let old_pixels = decoded(&before[i]);
        assert_color(&old_pixels, [255, 0, 0, 255]);
        assert!(style(&before[i]).attribution.iter().any(|a| a.contains("Old credits")));
        assert_render(&mut s, [255, 0, 0, 255], "Old credits");
        write_archive(&path, [0, 255, 0, 255], "New credits");
        // Existing retained data stays until the explicit command.
        assert!(Arc::ptr_eq(&decoded(&before[i]), &old_pixels));
        assert_eq!(s.run("map.reloadTiles", &json!({"id":"linked-archive"})).unwrap(), json!({"index":i,"path":path}));
        assert_eq!(s.map().tiles, before);
        assert_eq!(s.map().camera, camera);
        assert_color(&decoded(&before[i]), [0, 255, 0, 255]);
        let fresh = style(&before[i]);
        assert!(fresh.attribution.iter().any(|a| a.contains("New credits")));
        assert!(!fresh.attribution.iter().any(|a| a.contains("Old credits")));
        assert!(Arc::ptr_eq(&decoded(other_tile), &other_pixels));
        assert!(Arc::ptr_eq(&style(other_tile), &other_style));
        assert_render(&mut s, [0, 255, 0, 255], "New credits");
        assert_eq!(s.undo_label(), Some("Reload Tile Archive"));
        s.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(s.map().tiles, before);
        assert_color(&decoded(&before[i]), [0, 255, 0, 255]); // Undo is not file recovery.
        s.run("edit.redo", &Value::Null).unwrap();
        assert_eq!(s.map().camera, camera);
        // All three selector forms, including case-insensitive names.
        s.run("map.reloadTiles", &json!({"name":"lInKeD"})).unwrap();
        s.run("map.reloadTiles", &json!({"index":i})).unwrap();
        rustgis_engine::data::mbtiles::close(&path);
        rustgis_engine::data::mbtiles::close(&other);
    }
}

fn rejected(s: &mut Session, params: Value, expected: &str) {
    let before = s.map().tiles.clone();
    let undo = s.undo_label();
    let mark = s.notice_mark();
    let error = s.run("map.reloadTiles", &params).unwrap_err();
    assert!(error.contains(expected), "{error}");
    assert_eq!(s.map().tiles, before);
    assert_eq!(s.undo_label(), undo);
    assert!(s.notices_since(mark).iter().any(|n| n.error && n.text.contains(expected)));
}

#[test]
fn failed_archive_reload_retains_tiles_configuration_and_allows_retry() {
    for ext in ["pmtiles", "mbtiles"] {
        let f = Scoped::new(&format!("reload-failure-{ext}"));
        let path = f.file(&format!("linked.{ext}"));
        write_archive(&path, [255, 0, 0, 255], "Retained credits");
        let mut s = session();
        s.run("map.addTiles", &json!({"url":path})).unwrap();
        let before = s.map().tiles[0].clone();
        let pixels = decoded(&before);
        let old_style = style(&before);
        // No live read is held by this test while changing the file on Windows.
        rustgis_engine::data::mbtiles::close(&path);
        std::fs::remove_file(&path).unwrap();
        rejected(&mut s, json!({"index":0}), "retry Reload Data");
        std::fs::write(&path, b"truncated").unwrap();
        rejected(&mut s, json!({"index":0}), "retry Reload Data");
        assert!(Arc::ptr_eq(&decoded(&before), &pixels));
        assert!(Arc::ptr_eq(&style(&before), &old_style));
        write_archive_after_truncation(&path, [0, 255, 0, 255], "Recovered credits");
        s.run("map.reloadTiles", &json!({"index":0})).unwrap();
        assert_color(&decoded(&before), [0, 255, 0, 255]);
        assert_eq!(s.map().tiles[0], before);
        rustgis_engine::data::mbtiles::close(&path);
    }
}

fn write_archive_after_truncation(path: &Path, color: [u8; 4], credit: &str) {
    std::fs::remove_file(path).unwrap();
    write_archive(path, color, credit);
}

#[test]
fn locked_mbtiles_reload_fails_without_evicting_retained_data_then_retries() {
    let f = Scoped::new("reload-locked");
    let path = f.file("locked.mbtiles");
    write_archive(&path, [255, 0, 0, 255], "Before lock");
    let mut s = session();
    s.run("map.addTiles", &json!({"url":path})).unwrap();
    let before = s.map().tiles[0].clone();
    let pixels = decoded(&before);
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch("BEGIN EXCLUSIVE; UPDATE metadata SET value='After lock' WHERE name='attribution';").unwrap();
    rejected(&mut s, json!({"index":0}), "retry Reload Data");
    assert!(Arc::ptr_eq(&decoded(&before), &pixels));
    c.execute_batch("COMMIT").unwrap();
    s.run("map.reloadTiles", &json!({"index":0})).unwrap();
    assert!(style(&before).attribution.iter().any(|a| a.contains("After lock")));
    rustgis_engine::data::mbtiles::close(&path);
}

// Unix permits replacing an open archive's directory entry. Windows may refuse the rename
// while SQLite holds the original file; this test does not promise otherwise.
#[cfg(unix)]
#[test]
fn replacing_an_open_mbtiles_archive_reloads_the_new_file_not_its_pooled_inode() {
    let f = Scoped::new("reload-mbtiles-replaced-inode");
    let path = f.file("linked.mbtiles");
    let replacement = f.file("replacement.mbtiles");
    write_archive(&path, [255, 0, 0, 255], "Old inode");
    write_archive(&replacement, [0, 255, 0, 255], "Replacement inode");
    let mut s = session();
    s.run("map.addTiles", &json!({"url":path})).unwrap();
    let before = s.map().tiles[0].clone();
    assert_color(&decoded(&before), [255, 0, 0, 255]);
    assert!(style(&before).attribution.iter().any(|a| a.contains("Old inode")));
    std::fs::rename(&replacement, &path).unwrap();
    s.run("map.reloadTiles", &json!({"index":0})).unwrap();
    assert_eq!(s.map().tiles[0], before);
    assert_color(&decoded(&before), [0, 255, 0, 255]);
    assert!(style(&before).attribution.iter().any(|a| a.contains("Replacement inode")));
    rustgis_engine::data::mbtiles::close(&path);
}

#[test]
fn incompatible_reload_keeps_archive_presentation_and_undo_restores_prior_error() {
    for ext in ["pmtiles", "mbtiles"] {
        let f = Scoped::new(&format!("reload-incompatible-{ext}"));
        let path = f.file(&format!("vector.{ext}"));
        write_vector(&path);
        let mut s = session();
        s.run("map.addTiles", &json!({"url":path})).unwrap();
        s.map_mut().tiles[0].layers = vec!["places".into()];
        s.map_mut().tiles[0].style_layers =
            r##"[{"id":"places","type":"circle","source-layer":"places","paint":{"circle-color":"#2266aa"}}]"##.into();
        s.map_mut().tiles[0].hidden_layers = vec!["roads".into()];
        s.map_mut().tiles[0].opacity = 0.4;
        s.map_mut().tiles[0].visible = false;
        s.map_mut().tiles[0].blend = rustgis_engine::map::effects::Blend::Multiply;
        s.map_mut().tiles[0].id = "presentation".into();
        s.map_mut().tiles[0].error = Some("old unavailable archive".into());
        let before = s.map().tiles[0].clone();
        write_archive(&path, [0, 255, 0, 255], "Raster now");
        rejected(&mut s, json!({"index":0}), "raster tiles");
        write_vector(&path);
        s.run("map.reloadTiles", &json!({"index":0})).unwrap();
        let mut expected = before.clone();
        expected.error = None;
        assert_eq!(s.map().tiles[0], expected);
        s.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(s.map().tiles[0], before);
        rustgis_engine::data::mbtiles::close(&path);
    }
}

fn write_vector(path: &Path) {
    if path.extension().unwrap() == "pmtiles" {
        super::common::archive_file(path);
    } else {
        write_archive(path, [255, 0, 0, 255], "Vector");
        let c = rusqlite::Connection::open(path).unwrap();
        c.execute("UPDATE metadata SET value='pbf' WHERE name='format'", []).unwrap();
        let tile = rustgis_engine::render::rustgis_tiles::mvt::encode(&rustgis_engine::render::rustgis_tiles::mvt::Tile { layers: vec![] });
        c.execute("UPDATE tiles SET tile_data=?1", [tile]).unwrap();
    }
}

#[test]
fn archive_reload_rejects_remote_upload_and_invalid_selectors() {
    let mut s = session();
    s.run("map.addTiles", &json!({"url":"https://example.invalid/a.pmtiles"})).unwrap();
    rejected(&mut s, json!({"index":0}), "linked local PMTiles or MBTiles");
    // Uploaded archives are snapshots, not live paths.
    s.map_mut().tiles[0] = TileLayer::new("Snapshot", rustgis_engine::map::TileKind::MbTiles, "upload:local.mbtiles");
    rejected(&mut s, json!({"index":0}), "add snapshots/uploads again");
    for (p, expected) in [
        (json!({}), "required"),
        (json!({"index":-1}), "no tile layer"),
        (json!({"index":0.5}), "no tile layer"),
        (json!({"index":8}), "no tile layer"),
        (json!({"name":"absent"}), "no tile layer"),
        (json!({"id":"absent"}), "no tile layer"),
    ] {
        rejected(&mut s, p, expected);
    }
}

#[test]
fn invalid_raster_and_unreadable_tile_pages_do_not_evict_retained_mbtiles() {
    for damaged_page in [false, true] {
        let f = Scoped::new(if damaged_page { "reload-tile-page" } else { "reload-png-signature" });
        let path = f.file("linked.mbtiles");
        write_archive(&path, [255, 0, 0, 255], "Retained credits");
        let mut s = session();
        s.run("map.addTiles", &json!({"url":path,"overlay":true})).unwrap();
        let before = s.map().tiles[0].clone();
        let pixels = decoded(&before);
        let credits = style(&before);
        rustgis_engine::data::mbtiles::close(&path);
        let c = rusqlite::Connection::open(&path).unwrap();
        if damaged_page {
            let page_size = c.query_row("PRAGMA page_size", [], |r| r.get::<_, u32>(0)).unwrap() as usize;
            let root = c.query_row("SELECT rootpage FROM sqlite_master WHERE name='tiles'", [], |r| r.get::<_, u32>(0)).unwrap() as usize;
            drop(c);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[(root - 1) * page_size] = 0;
            std::fs::write(&path, bytes).unwrap();
            // Metadata is readable; this is not another damaged-header test.
            assert!(rustgis_engine::data::mbtiles::open_path(&path).is_ok());
            rustgis_engine::data::mbtiles::close(&path);
        } else {
            c.execute("UPDATE tiles SET tile_data=?1", [b"\x89PNG\r\n\x1a\n".as_slice()]).unwrap();
            drop(c);
        }
        rejected(&mut s, json!({"index":0}), "retry Reload Data");
        assert!(Arc::ptr_eq(&decoded(&before), &pixels));
        assert!(Arc::ptr_eq(&style(&before), &credits));
        write_archive_after_truncation(&path, [0, 255, 0, 255], "Recovered credits");
        s.run("map.reloadTiles", &json!({"index":0})).unwrap();
        assert_color(&decoded(&before), [0, 255, 0, 255]);
        rustgis_engine::data::mbtiles::close(&path);
    }
}

fn write_leaf_archive(path: &Path, corrupt: bool) {
    let archive = pmtiles::NewArchive {
        tile_type: pmtiles::TileType::Png,
        gzipped_tiles: false,
        bounds: [-180.0, -85.0, 180.0, 85.0],
        metadata: json!({"attribution":"Leaf credits"}),
    };
    let bytes = pmtiles::write(vec![(0, 0, 0, picture([0, 255, 0, 255]))], &archive).unwrap();
    let number = |at| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize;
    let mut leaf = bytes[number(8)..number(8) + number(16)].to_vec();
    if corrupt {
        // One entry's final offset varint has no terminating byte.
        *leaf.last_mut().unwrap() = 0x80;
    }
    assert!(leaf.len() < 128);
    let root = [1, 0, 0, leaf.len() as u8, 1];
    let meta = &bytes[number(24)..number(24) + number(32)];
    let data = &bytes[number(56)..number(56) + number(64)];
    let mut header = bytes[..127].to_vec();
    let meta_at = 127 + root.len();
    let leaf_at = meta_at + meta.len();
    let data_at = leaf_at + leaf.len();
    for (at, value) in
        [(8, 127), (16, root.len()), (24, meta_at), (32, meta.len()), (40, leaf_at), (48, leaf.len()), (56, data_at), (64, data.len())]
    {
        header[at..at + 8].copy_from_slice(&(value as u64).to_le_bytes());
    }
    std::fs::write(path, [header.as_slice(), root.as_slice(), meta, leaf.as_slice(), data].concat()).unwrap();
}

#[test]
fn malformed_leaf_reload_retains_pixels_and_can_retry_with_a_valid_leaf() {
    let f = Scoped::new("reload-malformed-leaf");
    let path = f.file("linked.pmtiles");
    write_archive(&path, [255, 0, 0, 255], "Retained credits");
    let mut s = session();
    s.run("map.addTiles", &json!({"url":path,"overlay":true})).unwrap();
    let before = s.map().tiles[0].clone();
    let pixels = decoded(&before);
    let credits = style(&before);
    let camera = s.map().camera;
    write_leaf_archive(&path, true);
    rejected(&mut s, json!({"index":0}), "retry Reload Data");
    assert!(Arc::ptr_eq(&decoded(&before), &pixels));
    assert!(Arc::ptr_eq(&style(&before), &credits));
    assert_eq!(s.map().camera, camera);
    write_leaf_archive(&path, false);
    s.run("map.reloadTiles", &json!({"index":0})).unwrap();
    assert_color(&decoded(&before), [0, 255, 0, 255]);
}

#[test]
fn untrusted_workflows_cannot_reload_but_user_authored_workflows_can() {
    let f = Scoped::new("reload-workflow-trust");
    let path = f.file("linked.pmtiles");
    write_archive(&path, [255, 0, 0, 255], "Before");
    let mut s = session();
    s.run("map.addTiles", &json!({"url":path,"overlay":true})).unwrap();
    let tile = s.map().tiles[0].clone();
    let old_pixels = decoded(&tile);
    let task = json!({"name":"Reload task","steps":[{"title":"Reload","command":"map.reloadTiles","params":{"index":0}}]});
    let error = s.run("task.import", &json!({"text":task.to_string()})).unwrap_err();
    assert!(error.contains("imported tasks"), "{error}");
    let notebook = json!({"nbformat":4,"metadata":{"language_info":{"name":"rustgis"}},"cells":[
        {"cell_type":"code","source":"map.reloadTiles {\"index\":0}"}
    ]});
    s.run("notebook.open", &json!({"text":notebook.to_string()})).unwrap();
    let answer = s.run("notebook.run", &Value::Null).unwrap();
    assert_eq!(answer["error"], 0);
    assert!(s.project.notebooks.last().unwrap().cells[0].error);
    assert!(Arc::ptr_eq(&decoded(&tile), &old_pixels));
    write_archive(&path, [0, 255, 0, 255], "After");
    s.run("task.new", &task).unwrap();
    s.run("task.run", &json!({"task":"Reload task"})).unwrap();
    assert_color(&decoded(&tile), [0, 255, 0, 255]);
    s.run("notebook.new", &json!({"name":"User reload"})).unwrap();
    s.run("notebook.addCell", &json!({"source":"map.reloadTiles {\"index\":0}"})).unwrap();
    let answer = s.run("notebook.run", &Value::Null).unwrap();
    assert!(answer.get("error").is_none(), "{answer}");
}

#[test]
fn successful_reload_cancels_only_its_own_pending_archive_framing() {
    let f = Scoped::new("reload-pending-view");
    let path = f.file("linked.pmtiles");
    let other = f.file("other.pmtiles");
    for file in [&path, &other] {
        let archive = pmtiles::NewArchive {
            tile_type: pmtiles::TileType::Png,
            gzipped_tiles: false,
            bounds: [20.0, 30.0, 25.0, 35.0],
            metadata: json!({}),
        };
        std::fs::write(file, pmtiles::write(vec![(0, 0, 0, picture([255, 0, 0, 255]))], &archive).unwrap()).unwrap();
    }
    let mut s = session();
    s.run("map.addTiles", &json!({"url":path,"overlay":true})).unwrap();
    s.run("map.addTiles", &json!({"url":other,"overlay":true})).unwrap();
    s.style_view = None;
    s.run("map.goToXY", &json!({"lon":-40,"lat":-20,"scale":50_000_000})).unwrap();
    let target = s.map().tiles.iter().position(|t| t.url == path.to_str().unwrap()).unwrap();
    let tile = s.map().tiles[target].clone();
    let camera = s.map().camera;
    cache::reload_archive(&tile.url);
    s.style_view = Some((tile.clone(), camera, s.map().instance_id()));
    s.run("map.reloadTiles", &json!({"index":target})).unwrap();
    assert!(s.style_view.is_none(), "late target metadata must no longer frame the view");
    let _ = style(&tile);
    assert!(!s.follow_style_view());
    assert_eq!(s.map().camera, camera);
    let other_tile = s.map().tiles.iter().find(|t| t.url != tile.url).unwrap().clone();
    cache::reload_archive(&other_tile.url);
    s.style_view = Some((other_tile.clone(), camera, s.map().instance_id()));
    s.run("map.reloadTiles", &json!({"index":target})).unwrap();
    assert!(s.style_view.as_ref().is_some_and(|(pending, _, _)| pending.url == other_tile.url));
    let _ = style(&other_tile);
    assert!(s.follow_style_view(), "another archive's requested framing still works");
    assert_ne!(s.map().camera, camera);
}

#[test]
fn prepared_archive_validation_does_not_evict_or_overwrite_changed_settings() {
    let f = Scoped::new("reload-read-ahead");
    let path = f.file("linked.pmtiles");
    write_archive(&path, [255, 0, 0, 255], "Before");
    let mut s = session();
    s.run("map.addTiles", &json!({"url":path,"overlay":true})).unwrap();
    let tile = s.map().tiles[0].clone();
    let old_pixels = decoded(&tile);
    write_archive(&path, [0, 255, 0, 255], "After");
    let control = rustgis_engine::analysis::control::Control::stoppable();
    let read = rustgis_engine::cmd::reads::read_ahead(&mut s, "map.reloadTiles", &json!({"index":0})).unwrap().run(&control).unwrap();
    assert!(Arc::ptr_eq(&decoded(&tile), &old_pixels), "validation alone must not invalidate");
    s.run("map.setTiles", &json!({"index":0,"opacity":0.4})).unwrap();
    let before = s.map().tiles.clone();
    let undo = s.undo_depth();
    assert!(rustgis_engine::cmd::reads::read_land(&mut s, read).is_err());
    assert_eq!(s.map().tiles, before);
    assert_eq!(s.undo_depth(), undo);
    assert_eq!(s.map().tiles[0].opacity, 0.4);
    assert!(Arc::ptr_eq(&decoded(&tile), &old_pixels));
    s.run("map.reloadTiles", &json!({"index":0})).unwrap();
    assert_eq!(s.map().tiles[0].opacity, 0.4);
    assert_color(&decoded(&tile), [0, 255, 0, 255]);
}

fn archive_read(s: &mut Session, command: &str, params: &Value) -> rustgis_engine::cmd::reads::Read {
    let control = rustgis_engine::analysis::control::Control::stoppable();
    rustgis_engine::cmd::reads::read_ahead(s, command, params).unwrap().run(&control).unwrap()
}

type RetainedArchive = (TileLayer, Arc<cache::Payload>, Arc<source::Resolved>);

fn retain_archive(t: &TileLayer) -> RetainedArchive {
    (t.clone(), decoded(t), style(t))
}

fn reject_stale_archive(s: &mut Session, read: rustgis_engine::cmd::reads::Read, retained: &[RetainedArchive]) -> String {
    let before = serde_json::to_value(&s.project).unwrap();
    let view = s.style_view.clone();
    let undo = s.undo_depth();
    let mark = s.notice_mark();
    let error = rustgis_engine::cmd::reads::read_land(s, read).unwrap_err();
    assert_eq!(serde_json::to_value(&s.project).unwrap(), before, "failed landing must retain source, presentation and view");
    assert_eq!(s.style_view, view, "failed landing must not cancel pending framing");
    assert_eq!(s.undo_depth(), undo);
    assert!(s.notices_since(mark).iter().all(|n| n.error), "failed landing must not emit a success notice");
    for (tile, pixels, credits) in retained {
        assert!(Arc::ptr_eq(&decoded(tile), pixels), "failed landing must not evict {}", tile.url);
        assert!(Arc::ptr_eq(&style(tile), credits), "failed landing must not evict metadata for {}", tile.url);
    }
    error
}

#[test]
fn changed_or_removed_archive_after_worker_validation_retains_data_and_retries() {
    for ext in ["pmtiles", "mbtiles"] {
        for command in ["map.reloadTiles", "map.repairTiles"] {
            for remove in [false, true] {
                let f = Scoped::new(&format!("archive-freshness-{ext}-{command}-{remove}"));
                let linked = f.file(&format!("linked.{ext}"));
                let replacement = f.file(&format!("replacement.{ext}"));
                write_archive(&linked, [255, 0, 0, 255], "Retained credits");
                write_archive(&replacement, [0, 0, 255, 255], "Replacement credits");
                let mut s = session();
                s.run("map.addTiles", &json!({"url":linked,"overlay":true})).unwrap();
                s.map_mut().tiles[0].opacity = 0.4;
                s.map_mut().tiles[0].expanded = true;
                let tile = s.map().tiles[0].clone();
                let mut candidate = tile.clone();
                let mut params = json!({"index":0});
                let path = if command == "map.repairTiles" {
                    params["path"] = json!(replacement);
                    candidate.url = replacement.display().to_string();
                    &replacement
                } else {
                    &linked
                };
                let retained = vec![retain_archive(&tile), retain_archive(&candidate)];
                s.style_view = Some((tile.clone(), s.map().camera, s.map().instance_id()));
                let read = archive_read(&mut s, command, &params);
                rustgis_engine::data::mbtiles::close(path);
                if remove {
                    std::fs::remove_file(path).unwrap();
                } else {
                    let len = std::fs::metadata(path).unwrap().len();
                    std::fs::OpenOptions::new().write(true).open(path).unwrap().set_len(len + 1).unwrap();
                }
                let error = reject_stale_archive(&mut s, read, &retained);
                assert!(error.contains("retry"), "{error}");
                let expected = if !remove {
                    "changed while it was read"
                } else if command == "map.repairTiles" {
                    "there's no such file"
                } else {
                    "observe archive metadata"
                };
                assert!(error.contains(expected), "{error}");
                if !remove {
                    std::fs::remove_file(path).unwrap();
                }
                write_archive(path, [0, 255, 0, 255], "Retry credits");
                s.run(command, &params).unwrap();
                assert_eq!(s.map().tiles[0].opacity, 0.4);
                assert!(s.map().tiles[0].expanded);
                assert_color(&decoded(&s.map().tiles[0]), [0, 255, 0, 255]);
                assert!(style(&s.map().tiles[0]).attribution.iter().any(|a| a.contains("Retry credits")));
                rustgis_engine::data::mbtiles::close(&linked);
                rustgis_engine::data::mbtiles::close(&replacement);
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn replacing_validated_archive_directory_entry_rejects_landing_even_with_identical_contents() {
    use std::os::unix::fs::MetadataExt;
    for ext in ["pmtiles", "mbtiles"] {
        for command in ["map.reloadTiles", "map.repairTiles"] {
            let f = Scoped::new(&format!("archive-freshness-inode-{ext}-{command}"));
            let linked = f.file(&format!("linked.{ext}"));
            let candidate_path = f.file(&format!("candidate.{ext}"));
            let swapped = f.file(&format!("swapped.{ext}"));
            write_archive(&linked, [255, 0, 0, 255], "Retained credits");
            write_archive(&candidate_path, [0, 0, 255, 255], "Candidate credits");
            let mut s = session();
            s.run("map.addTiles", &json!({"url":linked,"overlay":true})).unwrap();
            let tile = s.map().tiles[0].clone();
            let retained = vec![retain_archive(&tile)];
            s.style_view = Some((tile.clone(), s.map().camera, s.map().instance_id()));
            let mut params = json!({"index":0});
            let path = if command == "map.repairTiles" {
                params["path"] = json!(candidate_path);
                &candidate_path
            } else {
                &linked
            };
            let read = archive_read(&mut s, command, &params);
            let old = std::fs::metadata(path).unwrap();
            std::fs::copy(path, &swapped).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&swapped)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(old.modified().unwrap()))
                .unwrap();
            std::fs::rename(&swapped, path).unwrap();
            let new = std::fs::metadata(path).unwrap();
            assert_eq!(old.len(), new.len());
            assert_eq!(old.modified().unwrap(), new.modified().unwrap());
            assert_ne!(old.ino(), new.ino());
            assert!(reject_stale_archive(&mut s, read, &retained).contains("changed while it was read"));
            s.run(command, &params).unwrap();
            assert_eq!(s.map().tiles[0].url, path.display().to_string());
            rustgis_engine::data::mbtiles::close(&linked);
            rustgis_engine::data::mbtiles::close(&candidate_path);
        }
    }
}

#[test]
fn mbtiles_wal_commit_with_writer_open_invalidates_worker_validation_without_eviction() {
    for command in ["map.reloadTiles", "map.repairTiles"] {
        let f = Scoped::new(&format!("archive-freshness-wal-{command}"));
        let linked = f.file("linked.mbtiles");
        let replacement = f.file("replacement.mbtiles");
        write_archive(&linked, [255, 0, 0, 255], "Retained credits");
        write_archive(&replacement, [0, 0, 255, 255], "Replacement credits");
        let mut s = session();
        s.run("map.addTiles", &json!({"url":linked,"overlay":true})).unwrap();
        let tile = s.map().tiles[0].clone();
        let mut params = json!({"index":0});
        let path = if command == "map.repairTiles" {
            params["path"] = json!(replacement);
            &replacement
        } else {
            &linked
        };
        let writer = rusqlite::Connection::open(path).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; UPDATE metadata SET value='Before worker' WHERE name='attribution';",
            )
            .unwrap();
        let mut candidate = tile.clone();
        candidate.url = path.display().to_string();
        let retained = vec![retain_archive(&tile), retain_archive(&candidate)];
        s.style_view = Some((tile.clone(), s.map().camera, s.map().instance_id()));
        let read = archive_read(&mut s, command, &params);
        let before = std::fs::metadata(path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE; UPDATE metadata SET value='Committed after worker' WHERE name='attribution';").unwrap();
        writer.execute("UPDATE tiles SET tile_data=?1", [picture([0, 255, 0, 255])]).unwrap();
        writer.execute_batch("COMMIT").unwrap();
        let after = std::fs::metadata(path).unwrap();
        assert_eq!(before.len(), after.len(), "this regression must exercise a sidecar-only commit");
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        assert!(reject_stale_archive(&mut s, read, &retained).contains("changed while it was read"));
        // Keep the writer open: retry must see committed WAL contents, without requiring a checkpoint.
        s.run(command, &params).unwrap();
        assert_color(&decoded(&s.map().tiles[0]), [0, 255, 0, 255]);
        assert!(style(&s.map().tiles[0]).attribution.iter().any(|a| a.contains("Committed after worker")));
        drop(writer);
        rustgis_engine::data::mbtiles::close(&linked);
        rustgis_engine::data::mbtiles::close(&replacement);
    }
}

#[test]
fn clean_wal_mode_mbtiles_first_reload_and_repair_ignore_reader_created_empty_wal() {
    for command in ["map.reloadTiles", "map.repairTiles"] {
        let f = Scoped::new(&format!("archive-clean-wal-{command}"));
        let linked = f.file("linked.mbtiles");
        let replacement = f.file("replacement.mbtiles");
        write_archive(&linked, [255, 0, 0, 255], "Retained credits");
        write_archive(&replacement, [0, 255, 0, 255], "Clean WAL credits");
        let mut s = session();
        s.run("map.addTiles", &json!({"url":linked,"overlay":true})).unwrap();
        let mut params = json!({"index":0});
        let path = if command == "map.repairTiles" {
            params["path"] = json!(replacement);
            &replacement
        } else {
            &linked
        };
        // A clean close of the last WAL connection removes -wal/-shm; the archive stays WAL mode.
        let writer = rusqlite::Connection::open(path).unwrap();
        writer.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0)).unwrap();
        drop(writer);
        rustgis_engine::data::mbtiles::close(&linked);
        rustgis_engine::data::mbtiles::close(&replacement);
        for sidecar in ["-wal", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{sidecar}", path.display()));
        }
        assert!(!Path::new(&format!("{}-wal", path.display())).exists());
        // Whether or not the reader creates an empty -wal while validating, the first attempt lands.
        let read = archive_read(&mut s, command, &params);
        rustgis_engine::cmd::reads::read_land(&mut s, read).unwrap();
        let expected = if command == "map.repairTiles" { [0, 255, 0, 255] } else { [255, 0, 0, 255] };
        assert_color(&decoded(&s.map().tiles[0]), expected);
        if command == "map.repairTiles" {
            assert_eq!(s.map().tiles[0].url, path.display().to_string());
        }
        rustgis_engine::data::mbtiles::close(&linked);
        rustgis_engine::data::mbtiles::close(&replacement);
    }
}

#[test]
fn archive_observation_begins_in_worker_not_when_work_is_queued() {
    for ext in ["pmtiles", "mbtiles"] {
        let f = Scoped::new(&format!("archive-worker-observation-{ext}"));
        let path = f.file(&format!("linked.{ext}"));
        write_archive(&path, [255, 0, 0, 255], "Before queue");
        let mut s = session();
        s.run("map.addTiles", &json!({"url":path,"overlay":true})).unwrap();
        let ahead = rustgis_engine::cmd::reads::read_ahead(&mut s, "map.reloadTiles", &json!({"index":0})).unwrap();
        write_archive(&path, [0, 255, 0, 255], "Written before worker starts");
        let read = ahead.run(&rustgis_engine::analysis::control::Control::stoppable()).unwrap();
        rustgis_engine::cmd::reads::read_land(&mut s, read).unwrap();
        assert_color(&decoded(&s.map().tiles[0]), [0, 255, 0, 255]);
        rustgis_engine::data::mbtiles::close(&path);
    }
}

/// Move the writer's uncompressed root index into a leaf and optionally add an intermediate
/// directory. All pointer fields are small fixture constants; tile payloads remain unchanged.
fn pmtiles_with_leaf_pointer(bytes: &[u8], pointer_id: u8, nested: bool) -> Vec<u8> {
    let at = |offset: usize| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize;
    assert_eq!(bytes[97], 1, "fixture directories must be uncompressed");
    assert_eq!(at(48), 0, "fixture must start with no leaf directories");
    let mut leaves = bytes[at(8)..at(8) + at(16)].to_vec();
    let leaf_length = u8::try_from(leaves.len()).unwrap();
    let mut root = vec![1, pointer_id, 0, leaf_length, 1];
    if nested {
        root[3] = 5;
        root[4] = leaf_length + 1;
        leaves.extend([1, 0, 0, leaf_length, 1]);
    }
    let metadata = &bytes[at(24)..at(24) + at(32)];
    let data = &bytes[at(56)..at(56) + at(64)];
    let mut head = bytes[..127].to_vec();
    let metadata_at = 127 + root.len() as u64;
    let leaf_at = metadata_at + metadata.len() as u64;
    let data_at = leaf_at + leaves.len() as u64;
    for (offset, value) in [(8, 127), (16, root.len() as u64), (24, metadata_at), (40, leaf_at), (48, leaves.len() as u64), (56, data_at)] {
        head[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    [head, root, metadata.to_vec(), leaves, data.to_vec()].concat()
}

#[test]
fn unreachable_pmtiles_runs_reject_reload_and_repair_without_losing_retained_data() {
    for command in ["map.reloadTiles", "map.repairTiles"] {
        for defect in ["overlap", "direct lower bound", "nested lower bound"] {
            let f = Scoped::new(&format!("archive-unreachable-runs-{command}-{defect}"));
            let linked = f.file("linked.pmtiles");
            let replacement = f.file("replacement.pmtiles");
            write_archive(&linked, [255, 0, 0, 255], "Retained credits");
            let mut s = session();
            s.run("map.addTiles", &json!({"url":linked,"overlay":true})).unwrap();
            s.run("map.setTiles", &json!({"index":0,"opacity":0.4})).unwrap();
            let tile = s.map().tiles[0].clone();
            let retained = vec![retain_archive(&tile)];
            s.style_view = Some((tile.clone(), s.map().camera, s.map().instance_id()));
            let archive = pmtiles::NewArchive {
                tile_type: pmtiles::TileType::Png,
                gzipped_tiles: false,
                bounds: [-180.0, -85.0, 180.0, 85.0],
                metadata: json!({"attribution":"Valid reachable runs"}),
            };
            let valid = pmtiles::write(vec![(0, 0, 0, picture([0, 255, 0, 255])), (1, 0, 0, picture([0, 0, 255, 255]))], &archive).unwrap();
            assert_eq!(&valid[127..132], &[2, 0, 1, 1, 1], "fixture must contain two adjacent single-tile runs");
            let (invalid, corrected, expected) = if defect == "overlap" {
                let mut overlapping = valid.clone();
                overlapping[130] = 2; // First [0,1) run becomes [0,2), overlapping the next entry.
                (overlapping, valid, "overlapping PMTiles tile runs")
            } else {
                let nested = defect == "nested lower bound";
                (
                    pmtiles_with_leaf_pointer(&valid, 5, nested),
                    pmtiles_with_leaf_pointer(&valid, 0, nested),
                    "a PMTiles leaf directory contains tiles below its pointer",
                )
            };
            let mut params = json!({"index":0});
            let path = if command == "map.repairTiles" {
                params["path"] = json!(replacement);
                &replacement
            } else {
                &linked
            };
            std::fs::write(path, invalid).unwrap();
            let read = archive_read(&mut s, command, &params);
            assert!(reject_stale_archive(&mut s, read, &retained).contains(expected));
            std::fs::write(path, corrected).unwrap();
            s.run(command, &params).unwrap();
            let mut expected_tile = tile;
            expected_tile.url = path.display().to_string();
            assert_eq!(s.map().tiles[0], expected_tile, "retry must preserve presentation and update only the repaired path");
            assert_color(&decoded(&s.map().tiles[0]), [0, 255, 0, 255]);
            assert!(style(&s.map().tiles[0]).attribution.iter().any(|a| a.contains("Valid reachable runs")));
        }
    }
}
