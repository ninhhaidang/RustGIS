//! SRTM height tiles and DTED, added natively by path and as uploads (#2076).
use super::*;

/// A 1201 × 1201 SRTM tile: heights by row, one void.
fn hgt_tile() -> Vec<u8> {
    let mut b = Vec::with_capacity(1201 * 1201 * 2);
    for row in 0..1201usize {
        for col in 0..1201usize {
            let v: i16 = if (row, col) == (5, 5) { i16::MIN } else { (row % 300) as i16 };
            b.extend_from_slice(&v.to_be_bytes());
        }
    }
    b
}

/// A DTED level 1 cell of 1201 × 1201 posts at 3″ from 6°E, 45°S (MIL-PRF-89020B: UHL, DSI, ACC,
/// then one record a longitude line).
fn dt1_cell() -> Vec<u8> {
    let mut b = format!("UHL10060000E0450000S00300030NA  U  {:12}120112010{:24}", "", "").into_bytes();
    let mut dsi = vec![b' '; 648];
    dsi[..3].copy_from_slice(b"DSI");
    dsi[141..144].copy_from_slice(b"E96");
    dsi[144..149].copy_from_slice(b"WGS84");
    b.extend(dsi);
    let mut acc = vec![b' '; 2700];
    acc[..3].copy_from_slice(b"ACC");
    b.extend(acc);
    for i in 0..1201u16 {
        let mut rec = vec![0xaa, 0, (i >> 8) as u8, i as u8];
        rec.extend(i.to_be_bytes());
        rec.extend([0, 0]);
        for j in 0..1201u16 {
            // Negative heights in signed magnitude.
            let v = if j % 2 == 0 { j % 100 } else { 0x8000 | (j % 100) };
            rec.extend(v.to_be_bytes());
        }
        let sum: u32 = rec.iter().map(|b| u32::from(*b)).sum();
        rec.extend(sum.to_be_bytes());
        b.extend(rec);
    }
    b
}

/// The layer `result` names: its raster's extent and pixel size.
fn placed(s: &Session, result: &Value) -> (rustgis_data::Extent, u32, u32) {
    let layer = s.map().layer(result["layer"].as_u64().unwrap() as u32).unwrap();
    let r = layer.raster.as_ref().unwrap().data.as_ref().unwrap();
    assert!(r.georeferenced);
    assert_eq!(r.crs, rustgis_data::raster::Crs::LonLat);
    (r.extent(), r.width, r.height)
}

fn near(e: rustgis_data::Extent, [w, s, east, n]: [f64; 4]) -> bool {
    [(e.min_x, w), (e.min_y, s), (e.max_x, east), (e.max_y, n)].iter().all(|(a, b)| (a - b).abs() < 1e-9)
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn srtm_and_dted_are_added_by_path_with_their_extent() {
    let dir = std::env::temp_dir().join(format!("rustgis-elevation-tiles-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let half = 1.5 / 3600.0;
    for (name, bytes, bbox) in [
        ("N37W122.hgt", hgt_tile(), [-122.0 - half, 37.0 - half, -121.0 + half, 38.0 + half]),
        ("S45E006.dt1", dt1_cell(), [6.0 - half, -45.0 - half, 7.0 + half, -44.0 + half]),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        let mut s = Session::new(Project::default());
        let result = s.run("layer.addData", &json!({"path": path})).unwrap();
        let (extent, w, h) = placed(&s, &result);
        assert_eq!((w, h), (1201, 1201), "{name}");
        assert!(near(extent, bbox), "{name}: {extent:?}");
        // The vertical reference is said once, as the layer is added.
        assert_eq!(s.notices.iter().filter(|n| n.text.contains("EGM96 geoid")).count(), 1, "{name}: {:?}", s.notices);
        let voids = s.notices.iter().filter(|n| n.text.contains("void")).count();
        assert_eq!(voids, usize::from(name.ends_with(".hgt")), "{name}: {:?}", s.notices);
    }
    // A tile's name places it: one that doesn't is refused, saying so, not handed to GDAL.
    let unnamed = dir.join("srtm.hgt");
    std::fs::write(&unnamed, hgt_tile()).unwrap();
    let e = Session::new(Project::default()).run("layer.addData", &json!({"path": unnamed})).unwrap_err().to_string();
    assert!(e.contains("doesn't say where the tile is"), "{e}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn bytes_past_a_dted_files_last_column_are_said_when_read_by_path() {
    let dir = std::env::temp_dir().join(format!("rustgis-dted-trailing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("S45E006.dt1");
    for (extra, said) in [(0, false), (7, true)] {
        let mut bytes = dt1_cell();
        bytes.extend(std::iter::repeat_n(0, extra));
        std::fs::write(&path, bytes).unwrap();
        let mut s = Session::new(Project::default());
        s.run("layer.addData", &json!({"path": path})).unwrap();
        let notice = s.notices.iter().any(|n| n.text.contains("7 bytes after its last column are ignored"));
        let any = s.notices.iter().any(|n| n.text.contains("after its last column"));
        assert_eq!((notice, any), (said, said), "{extra}: {:?}", s.notices);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_srtm_upload_keeps_its_name_and_place() {
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD.encode(hgt_tile());
    let mut s = Session::new(Project::default());
    let result = s.run("layer.addDataBytes", &json!({"fileName": "S01E006.hgt", "data": data})).unwrap();
    let (extent, _, _) = placed(&s, &result);
    let half = 1.5 / 3600.0;
    assert!(near(extent, [6.0 - half, -1.0 - half, 7.0 + half, 0.0 + half]), "{extent:?}");
    assert!(s.notices.iter().any(|n| n.text.contains("1 of its 1442401 heights is a void")), "{:?}", s.notices);
    // A cut-short upload is refused.
    let cut = base64::engine::general_purpose::STANDARD.encode(&hgt_tile()[..1000]);
    assert!(Session::new(Project::default()).run("layer.addDataBytes", &json!({"fileName": "S01E006.hgt", "data": cut})).is_err());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_crs_or_subdataset_given_for_srtm_or_dted_is_said_and_ignored() {
    use base64::Engine;
    let dir = std::env::temp_dir().join(format!("rustgis-elevation-options-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let hgt = dir.join("N37W122.hgt");
    std::fs::write(&hgt, hgt_tile()).unwrap();
    let dt1 = dir.join("S45E006.dt1");
    std::fs::write(&dt1, dt1_cell()).unwrap();
    // As a project saved when these went through GDAL reopens them: read, the options said.
    let r = rustgis_data::raster::read_path_with(&hgt, Some("band2"), Some("EPSG:32610")).unwrap();
    assert_eq!((r.width, r.crs), (1201, rustgis_data::raster::Crs::LonLat));
    let both = "the \"layer\" and \"crs\" given are ignored: SRTM and DTED files hold one band of heights, with no layers to choose from, and give their own coordinates, read as WGS 84 longitude/latitude (a DTED file naming another datum says so)";
    assert!(r.lost.iter().any(|l| l == both), "{:?}", r.lost);
    let r = rustgis_data::raster::read_path_with(&dt1, None, Some("EPSG:4326")).unwrap();
    let crs = "the \"crs\" given is ignored: SRTM and DTED files give their own coordinates, read as WGS 84 longitude/latitude (a DTED file naming another datum says so)";
    assert!(r.lost.iter().any(|l| l == crs), "{:?}", r.lost);
    let r = rustgis_data::raster::read_path_with(&dt1, None, None).unwrap();
    assert!(!r.lost.iter().any(|l| l.contains("ignored")), "{:?}", r.lost);
    // From a command, by path and as an upload.
    let mut s = Session::new(Project::default());
    s.run("layer.addData", &json!({"path": hgt, "layer": "x"})).unwrap();
    let layer = "the \"layer\" given is ignored: SRTM and DTED files hold one band of heights, with no layers to choose from";
    assert!(s.notices.iter().any(|n| n.text.ends_with(layer)), "{:?}", s.notices);
    let data = base64::engine::general_purpose::STANDARD.encode(dt1_cell());
    let mut s = Session::new(Project::default());
    s.run("layer.addDataBytes", &json!({"fileName": "S45E006.dt1", "data": data, "crs": "EPSG:3857"})).unwrap();
    assert!(s.notices.iter().any(|n| n.text.ends_with(crs)), "{:?}", s.notices);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A project saved when SRTM and DTED went through GDAL keeps the coordinate system given then:
/// reopened, it is said to be ignored once and isn't kept, so the project saved again opens
/// without saying it.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_saved_crs_for_srtm_is_said_once_and_not_kept() {
    let dir = std::env::temp_dir().join(format!("rustgis-elevation-saved-crs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let hgt = dir.join("N37W122.hgt");
    std::fs::write(&hgt, hgt_tile()).unwrap();
    let dt1 = dir.join("S45E006.dt1");
    std::fs::write(&dt1, dt1_cell()).unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/fixtures/elevation_tiles.rgis");
    let text = std::fs::read_to_string(fixture).unwrap();
    assert!(text.contains(r#""crs":"EPSG:4326""#), "the fixture keeps a crs for the SRTM layer");
    let text = text
        .replace(r#""path":"N37W122.hgt""#, &format!("\"path\":{}", json!(hgt.display().to_string())))
        .replace(r#""path":"S45E006.dt1""#, &format!("\"path\":{}", json!(dt1.display().to_string())));
    let ignored = |s: &Session| s.notices.iter().filter(|n| n.text.contains("\"crs\" given is ignored")).count();
    let crs = |s: &Session, name: &str| s.map().layer_by_name(name).and_then(|l| l.raster.as_ref()).map(|r| r.crs.clone());

    let mut s = Session::new(Project::default());
    s.run("project.openText", &json!({"text": text})).unwrap();
    assert_eq!(ignored(&s), 1, "{:?}", s.notices);
    assert_eq!(crs(&s, "N37W122"), Some(None));
    assert_eq!(crs(&s, "S45E006"), Some(None));

    // Saved again and reopened: nothing to say.
    let saved = serde_json::to_string(&s.project).unwrap();
    let mut t = Session::new(Project::default());
    t.run("project.openText", &json!({"text": saved})).unwrap();
    assert_eq!(ignored(&t), 0, "{:?}", t.notices);
    assert!(t.map().layer_by_name("N37W122").and_then(|l| l.raster.as_ref()).and_then(|r| r.data.as_ref()).is_some());
    std::fs::remove_dir_all(dir).unwrap();
}
