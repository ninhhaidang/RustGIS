//! Never silent, for the rasters whose coordinate system is assumed, kept or turned on the way in
//! (#2008): what reading says reaches the session as a notice, and a raster whose file gives its
//! system and needs no resampling gets none of them.
use rustgis_engine::data::raster::{Crs, planetary, wkt};
use rustgis_engine::{Project, Session};
use serde_json::{Value, json};

/// The notices `command` gave when it added a raster layer with `params`, and that layer's
/// coordinate system.
fn add(command: &str, params: Value) -> (Vec<String>, Crs) {
    let mut s = Session::new(Project::default());
    let mark = s.notice_mark();
    let added = s.run(command, &params).unwrap_or_else(|e| panic!("{command}: {e}"));
    let id = added["layer"].as_u64().unwrap() as u32;
    let crs = s.map().layer(id).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().crs;
    (s.notices_since(mark).iter().map(|n| n.text.clone()).collect(), crs)
}

fn folder(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rustgis-raster-crs-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    dir
}

fn count(said: &[String], words: &str) -> usize {
    said.iter().filter(|n| n.contains(words)).count()
}

/// An ASCII grid with no `.prj` is read as WGS 84 and says so, once, naming the layer; one whose
/// `.prj` gives WGS 84 says nothing of the kind.
#[test]
fn an_ascii_grid_without_a_system_says_it_is_read_as_wgs84() {
    let dir = folder("ascii");
    let asc = dir.join("grid.asc");
    std::fs::write(&asc, "ncols 2\nnrows 2\nxllcorner 10\nyllcorner 40\ncellsize 1\n1 2\n3 4\n").unwrap_or_else(|e| panic!("{e}"));
    let (said, crs) = add("layer.addData", json!({"path": asc}));
    assert_eq!(crs, Crs::LonLat);
    assert_eq!(count(&said, "it doesn't say its coordinate system"), 1, "{said:?}");
    assert!(
        said.iter().any(|n| n.starts_with("grid: it doesn't say its coordinate system") && n.ends_with("so it's read as WGS 84")),
        "{said:?}"
    );
    std::fs::write(dir.join("grid.prj"), wkt::to_wkt(Crs::LonLat).unwrap()).unwrap();
    let (said, crs) = add("layer.addData", json!({"path": asc}));
    assert_eq!(crs, Crs::LonLat);
    assert_eq!(count(&said, "doesn't say its coordinate system"), 0, "{said:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A 20 × 10 byte GeoTIFF in UTM zone 33N placed by a transformation matrix (`m`, row by row), as
/// GDAL writes one: one strip, uncompressed.
fn matrix_tiff(m: [f64; 16]) -> Vec<u8> {
    let mut out = vec![b'I', b'I', 42, 0, 0, 0, 0, 0];
    out.extend((0..200u32).map(|i| i as u8 + 1));
    let shorts = |v: &[u16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let longs = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let doubles = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let mut entries: Vec<(u16, u16, u32, Vec<u8>)> = vec![
        (256, 4, 1, longs(&[20])),
        (257, 4, 1, longs(&[10])),
        (258, 3, 1, shorts(&[8])),
        (259, 3, 1, shorts(&[1])),
        (262, 3, 1, shorts(&[1])),
        (273, 4, 1, longs(&[8])),
        (277, 3, 1, shorts(&[1])),
        (278, 4, 1, longs(&[10])),
        (279, 4, 1, longs(&[200])),
        (34264, 12, 16, doubles(&m)),
        (34735, 3, 16, shorts(&[1, 1, 0, 3, 1024, 0, 1, 1, 1025, 0, 1, 1, 3072, 0, 1, 32633])),
    ];
    let ifd_at = out.len();
    out[4..8].copy_from_slice(&(ifd_at as u32).to_le_bytes());
    let values_at = ifd_at + 2 + 12 * entries.len() + 4;
    let (mut dir, mut values) = ((entries.len() as u16).to_le_bytes().to_vec(), Vec::<u8>::new());
    for (tag, kind, n, bytes) in &mut entries {
        dir.extend(tag.to_le_bytes());
        dir.extend(kind.to_le_bytes());
        dir.extend(n.to_le_bytes());
        if bytes.len() <= 4 {
            bytes.resize(4, 0);
            dir.extend(bytes.iter());
        } else {
            dir.extend(((values_at + values.len()) as u32).to_le_bytes());
            values.extend(bytes.iter());
        }
    }
    out.extend(dir);
    out.extend([0u8; 4]);
    out.extend(values);
    out
}

/// A rotated GeoTIFF is resampled onto a north-up grid, and says so once; the same image placed
/// north up is read as it is, and says nothing.
#[test]
fn a_rotated_geotiff_says_it_was_resampled() {
    let dir = folder("rotated");
    let rotated = [30.0, 5.0, 0.0, 400_000.0, 3.0, -30.0, 0.0, 5_000_000.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
    let level = [30.0, 0.0, 0.0, 400_000.0, 0.0, -30.0, 0.0, 5_000_000.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
    for (file, m, turned) in [("rotated.tif", rotated, 1), ("level.tif", level, 0)] {
        let path = dir.join(file);
        std::fs::write(&path, matrix_tiff(m)).unwrap_or_else(|e| panic!("{e}"));
        let (said, crs) = add("layer.addData", json!({"path": path}));
        assert_eq!(crs, Crs::Epsg(32633), "{file}");
        assert_eq!(count(&said, "resampled (nearest pixel) onto a north-up grid"), turned, "{file}: {said:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A Zarr store with longitude/latitude axes is in the geographic system it gives: ED50 (with its
/// shift) isn't WGS 84, another body's says so once, WGS 84's says nothing.
#[test]
fn a_zarr_store_keeps_its_geographic_system_and_says_what_it_left_out() {
    let coords = |dim: &str| {
        json!({"zarr_format": 3, "node_type": "array", "shape": [2], "data_type": "float64",
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [2]}},
            "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}], "fill_value": 0, "dimension_names": [dim]})
    };
    let f64s = |v: [f64; 2]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let ed50 = r#"GEOGCS["ED50",DATUM["European_Datum_1950",SPHEROID["International 1924",6378388,297],TOWGS84[-87,-98,-121,0,0,0,0]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]"#;
    let mars = r#"GEOGCS["Mars 2000",DATUM["D_Mars_2000",SPHEROID["Mars_2000_IAU_IAG",3396190,169.89444722361179]],PRIMEM["Reference_Meridian",0],UNIT["Degree",0.0174532925199433]]"#;
    for (tag, crs_wkt) in [("ed50", ed50), ("mars", mars), ("wgs84", "EPSG:4326")] {
        let dir = folder(&format!("zarr-{tag}"));
        let elev = json!({"zarr_format": 3, "node_type": "array", "shape": [2, 2], "data_type": "int16",
            "chunk_grid": {"name": "regular", "configuration": {"chunk_shape": [2, 2]}},
            "chunk_key_encoding": {"name": "default", "configuration": {"separator": "/"}},
            "codecs": [{"name": "bytes", "configuration": {"endian": "little"}}],
            "fill_value": 0, "dimension_names": ["lat", "lon"], "attributes": {"crs_wkt": crs_wkt}});
        let root = json!({"zarr_format": 3, "node_type": "group", "consolidated_metadata": {"kind": "inline", "metadata": {
            "elev": elev, "lat": coords("lat"), "lon": coords("lon")}}});
        let files = [
            ("zarr.json", root.to_string().into_bytes()),
            ("elev/c/0/0", [1i16, 2, 3, 4].iter().flat_map(|v| v.to_le_bytes()).collect()),
            ("lat/c/0", f64s([43.1, 43.0])),
            ("lon/c/0", f64s([-79.1, -79.0])),
        ];
        for (name, bytes) in files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap_or(&dir)).unwrap_or_else(|e| panic!("{e}"));
            std::fs::write(path, bytes).unwrap_or_else(|e| panic!("{e}"));
        }
        let (said, crs) = add("layer.addZarr", json!({"path": dir, "variable": "elev"}));
        let body = count(&said, "belongs to another body than the Earth");
        match tag {
            "ed50" => assert!(crs != Crs::LonLat && crs.is_geographic() && !crs.is_planetary() && body == 0, "{crs:?} {said:?}"),
            "mars" => assert!(crs.is_planetary() && crs != Crs::LonLat && body == 1, "{crs:?} {said:?}"),
            _ => assert!(crs == Crs::LonLat && body == 0, "{crs:?} {said:?}"),
        }
        assert_eq!(planetary::note_of(crs).is_some(), tag == "mars", "{tag}");
        assert_eq!(count(&said, "no conversion to WGS 84"), 0, "{tag}: {said:?}");
        let mut s = Session::new(Project::default());
        let result = s.run("layer.addZarr", &json!({"path": dir, "variable": "elev", "cells": [0, 0, 2, 2]}));
        if tag == "wgs84" {
            let id = result.unwrap()["layer"].as_u64().unwrap() as u32;
            let raster = s.map().layer(id).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap();
            assert_eq!(raster.bands[0].data, [1.0, 2.0, 3.0, 4.0]);
        } else {
            let error = result.unwrap_err();
            assert!(error.contains("can't be kept in a project") && error.contains("no layer was added"), "{tag}: {error}");
            assert!(s.map().layers.is_empty(), "{tag}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
