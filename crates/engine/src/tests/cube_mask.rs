//! Cloud-free summaries of a STAC collection: each date's cells masked by the same item's quality
//! band, then the median or the first clear date (`cube.reduce`'s `mask`, `median`, `first` and
//! `order`), over a small collection served here.
use super::*;

/// `size` × `size` cells over [10, 11] × [45, 46], in degrees, as `values` (row by row) say.
fn cog(values: Vec<f32>, size: u32, nodata: Option<f64>) -> Vec<u8> {
    cog_of(values, size, size, 1.0 / f64::from(size), nodata)
}

/// `width` × `height` cells of `px` degrees from (10, 46), as `values` (row by row) say.
fn cog_of(values: Vec<f32>, width: u32, height: u32, px: f64, nodata: Option<f64>) -> Vec<u8> {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let t = GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: px, pixel_height: -px };
    let r = Raster::new("r", width, height, vec![values], t, Crs::LonLat, nodata, ColorHint::Gray, "Float32").unwrap();
    rustgis_data::raster::cogwrite::write(&r, 0).unwrap()
}

/// Date k's red value at cell (col, row): 100 (k + 1) plus the cell's place, so each date's
/// value tells which it is.
fn red(k: usize, col: usize, row: usize) -> f32 {
    (100 * (k + 1) + row * 8 + col) as f32
}

/// Three dates of a red band and its SCL-like mask (4 clear, 9 cloud, 0 no-data): the first
/// clear in the west half (a share of 0.5), the second everywhere but the two north rows, on a
/// mask of twice the band's cells (4 × 4, read onto it by nearest neighbour: 0.75), the third
/// only in the two east columns, no-data elsewhere (0.25). A fourth item, the earliest, has no
/// mask and is left out. The cells no date has clear (two north rows, columns 4 and 5) are empty.
fn collection() -> (String, String) {
    let band = |k: usize| cog((0..64).map(|c| red(k, c % 8, c / 8)).collect(), 8, None);
    let masks = [
        cog((0..64).map(|c| if c % 8 < 4 { 4.0 } else { 9.0 }).collect(), 8, Some(0.0)),
        cog((0..16).map(|c| if c / 4 == 0 { 9.0 } else { 4.0 }).collect(), 4, Some(0.0)),
        cog((0..64).map(|c| if c % 8 >= 6 { 4.0 } else { 0.0 }).collect(), 8, Some(0.0)),
    ];
    let mut files: Vec<(String, Vec<u8>)> = (0..4).map(|k| (format!("/b{k}.tif"), band(k))).collect();
    files.extend(masks.into_iter().enumerate().map(|(k, m)| (format!("/m{k}.tif"), m)));
    let (files, _) = serve_ranged_files(files);
    let dates = ["2024-01-01", "2024-01-17", "2024-02-02", "2023-12-16"];
    let features: Vec<String> = dates
        .iter()
        .enumerate()
        .map(|(k, d)| {
            let scl = if k < 3 { format!(r#", "SCL": {{"href": "{files}/m{k}.tif"}}"#) } else { String::new() };
            format!(
                r#"{{"type": "Feature", "id": "s{k}", "collection": "s2", "bbox": [10, 45, 11, 46], "geometry": null,
                   "properties": {{"datetime": "{d}T00:00:00Z", "bdc:tiles": ["t1"]}},
                   "assets": {{"B04": {{"href": "{files}/b{k}.tif"}}{scl}}}}}"#
            )
        })
        .collect();
    let search = format!(r#"{{"type": "FeatureCollection", "features": [{}]}}"#, features.join(","));
    (format!("{}/v1", serve_routes(vec![("/v1/search", search)], 64)), files)
}

/// Which dates are clear at cell (col, row), in date order.
fn clear(col: usize, row: usize) -> Vec<usize> {
    [col < 4, row >= 2, col >= 6].iter().enumerate().filter(|(_, c)| **c).map(|(k, _)| k).collect()
}

#[test]
fn a_cloud_free_summary_of_a_stac_collection() {
    use crate::cmd::stac::PIECE_VALUES;
    let (api, _) = collection();
    let mut s = Session::new(Project::default());
    let scl = json!({"asset": "SCL", "keep": [4, 5, 6]});
    let base = json!({"url": api, "collection": "s2", "asset": "B04", "bbox": [10, 45, 11, 46], "start": "2023-12-01", "end": "2024-02-29", "mask": scl});
    let with = |extra: Value| {
        let mut p = base.clone();
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        p
    };
    let reduce = |s: &mut Session, p: &Value| -> Vec<f32> {
        let r = s.run("cube.reduce", p).unwrap_or_else(|e| panic!("{p}: {e}"));
        let l = s.map().layer(r["layers"][0].as_u64().unwrap() as u32).unwrap();
        let raster = l.raster.as_ref().unwrap().data.clone().unwrap();
        assert_eq!((raster.width, raster.height), (8, 8));
        raster.bands[0].data.clone()
    };
    let check = |got: &[f32], want: &dyn Fn(usize, usize) -> Option<f32>, what: &str| {
        for (c, v) in got.iter().enumerate() {
            let (col, row) = (c % 8, c / 8);
            match want(col, row) {
                Some(w) => assert!((v - w).abs() < 1e-3, "{what} ({col}, {row}): {v} vs {w}"),
                None => assert!(v.is_nan(), "{what} ({col}, {row}): {v}, not empty"),
            }
        }
    };
    // The best pixel, the clearest date first (the default with a mask): the second date, then the
    // first, then the third; the item without a mask left out, and said.
    let mark = s.notice_mark();
    let first = reduce(&mut s, &with(json!({"stat": "first"})));
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(said.iter().any(|t| t.contains("1 item(s) left out") && t.contains("SCL")), "{said:?}");
    assert!(!said.iter().any(|t| t.contains("ignores") || t.contains("outside")), "{said:?}");
    let by = |order: &'static [usize]| {
        move |col: usize, row: usize| order.iter().find(|k| clear(col, row).contains(k)).map(|k| red(*k, col, row))
    };
    check(&first, &by(&[1, 0, 2]), "first, clearest");
    // In date order and latest first.
    check(&reduce(&mut s, &with(json!({"stat": "first", "order": "date"}))), &by(&[0, 1, 2]), "first, by date");
    check(&reduce(&mut s, &with(json!({"stat": "first", "order": "-date"}))), &by(&[2, 1, 0]), "first, latest");
    // The clear dates' median (the mean of two), and their mean.
    let middle = |col: usize, row: usize| {
        let v: Vec<f32> = clear(col, row).iter().map(|k| red(*k, col, row)).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f32>() / v.len() as f32)
    };
    check(&reduce(&mut s, &with(json!({"stat": "median"}))), &middle, "median");
    check(&reduce(&mut s, &with(json!({"stat": "mean"}))), &middle, "mean");
    // Dropping the cloud class instead: the third date's no-data never lets a cell through.
    let drop = json!({"stat": "first", "order": "date", "mask": {"asset": "scl", "drop": [8, 9, 10]}});
    check(&reduce(&mut s, &with(drop)), &by(&[0, 1, 2]), "first, dropping clouds");
    // Read in two pieces of four rows: the same, the clear shares being the whole area's.
    PIECE_VALUES.with(|v| v.set(Some(8 * 4 * 3)));
    let in_pieces = reduce(&mut s, &with(json!({"stat": "first"})));
    let median_in_pieces = reduce(&mut s, &with(json!({"stat": "median"})));
    PIECE_VALUES.with(|v| v.set(None));
    check(&in_pieces, &by(&[1, 0, 2]), "first in pieces");
    check(&median_in_pieces, &middle, "median in pieces");
    // Unmasked, every item counts: first takes the earliest date, the fourth item's, and the
    // median of four.
    let mut plain = with(json!({"stat": "first"}));
    plain.as_object_mut().unwrap().remove("mask");
    check(&reduce(&mut s, &plain), &|col, row| Some(red(3, col, row)), "unmasked first");
    plain["stat"] = json!("median");
    check(&reduce(&mut s, &plain), &|col, row| Some((red(1, col, row) + red(2, col, row)) / 2.0), "unmasked median");
    // What can't be asked: first by clear share without a mask, once the cube is found unmasked;
    // the others before anything is read but the search.
    plain["stat"] = json!("first");
    plain["order"] = json!("clear");
    let e = s.run("cube.reduce", &plain).unwrap_err();
    assert!(e.contains("give \"mask\""), "{e}");
    for (bad, said) in [
        (json!({"stat": "mean", "order": "date"}), "order is first's"),
        (json!({"stat": "first", "order": "cloud"}), "order is clear, date or -date"),
        (json!({"stat": "first", "mask": {"asset": "SCL", "keep": []}}), "keep is a list"),
        (json!({"stat": "first", "mask": {"asset": "QA", "keep": [1]}}), "no item of s2 has both"),
    ] {
        let e = s.run("cube.reduce", &with(bad.clone())).unwrap_err();
        assert!(e.contains(said), "{bad}: {e}");
    }
}

/// The median and the first value of each cell over a stack of layers (no mask: a stack's layers
/// have no quality band), and a mask asked of one refused.
#[test]
fn median_and_first_of_a_stack() {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
    let mut s = Session::new(Project::default());
    let mut names = Vec::new();
    // Three layers of 2 × 1 cells; the second's first cell has no value.
    let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
    for (date, a, b) in [("2020-01-01", 3.0, 10.0), ("2020-02-01", f32::NAN, 20.0), ("2020-03-01", 1.0, 40.0)] {
        let name = format!("ndvi_{date}");
        let r = Raster::new(&name, 2, 1, vec![vec![a, b]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let bytes = geotiff::write(&r, 0).unwrap();
        crate::cmd::raster::add_raster(&mut s, &name, rustgis_map::RasterLayer::from_bytes(r, format!("{name}.tif"), bytes)).unwrap();
        names.push(json!(name));
    }
    let get = |s: &mut Session, p: Value| -> Vec<f32> {
        let r = s.run("cube.reduce", &p).unwrap_or_else(|e| panic!("{p}: {e}"));
        s.map().layer(r["layers"][0].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.as_ref().unwrap().bands[0]
            .data
            .clone()
    };
    assert_eq!(get(&mut s, json!({"layers": names, "stat": "median"})), [2.0, 20.0]);
    assert_eq!(get(&mut s, json!({"layers": names, "stat": "first"})), [3.0, 10.0]);
    assert_eq!(get(&mut s, json!({"layers": names, "stat": "first", "order": "-date"})), [1.0, 40.0]);
    let e = s.run("cube.reduce", &json!({"layers": names, "stat": "first", "mask": {"asset": "SCL", "keep": [4]}})).unwrap_err();
    assert!(e.contains("STAC collection"), "{e}");
    let e = s.run("cube.reduce", &json!({"layers": names, "stat": "first", "order": "clear"})).unwrap_err();
    assert!(e.contains("give \"mask\""), "{e}");
    let e = s.run("zarr.reduce", &json!({"layer": names[0], "stat": "median"})).unwrap_err();
    assert!(e.contains("cube.reduce's"), "{e}");
}

/// A band pixel its item's mask file doesn't cover (masks of the west half only) is left out as
/// not clear, and said with the count and the mask.
#[test]
fn band_pixels_outside_their_mask_are_said() {
    let band = |k: usize| cog((0..64).map(|c| red(k, c % 8, c / 8)).collect(), 8, None);
    let west = || cog_of(vec![4.0; 32], 4, 8, 0.125, Some(0.0));
    let (files, _) = serve_ranged_files(vec![
        ("/b0.tif".into(), band(0)),
        ("/b1.tif".into(), band(1)),
        ("/m0.tif".into(), west()),
        ("/m1.tif".into(), west()),
    ]);
    let features: Vec<String> = ["2024-01-01", "2024-01-17"]
        .iter()
        .enumerate()
        .map(|(k, d)| {
            format!(
                r#"{{"type": "Feature", "id": "s{k}", "collection": "s2", "bbox": [10, 45, 11, 46], "geometry": null,
                   "properties": {{"datetime": "{d}T00:00:00Z", "bdc:tiles": ["t1"]}},
                   "assets": {{"B04": {{"href": "{files}/b{k}.tif"}}, "SCL": {{"href": "{files}/m{k}.tif"}}}}}}"#
            )
        })
        .collect();
    let search = format!(r#"{{"type": "FeatureCollection", "features": [{}]}}"#, features.join(","));
    let api = format!("{}/v1", serve_routes(vec![("/v1/search", search)], 8));
    let mut s = Session::new(Project::default());
    let mark = s.notice_mark();
    let p = json!({"url": api, "collection": "s2", "asset": "B04", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-01-31",
        "mask": {"asset": "SCL", "keep": [4]}, "stat": "first", "order": "date"});
    let r = s.run("cube.reduce", &p).unwrap();
    let data = s.map().layer(r["layers"][0].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    for (c, v) in data.bands[0].data.iter().enumerate() {
        let (col, row) = (c % 8, c / 8);
        if col < 4 {
            assert_eq!(*v, red(0, col, row), "({col}, {row})");
        } else {
            assert!(v.is_nan(), "({col}, {row}): {v}");
        }
    }
    // 32 pixels a date, two dates, said once; every item has its mask, so none is said left out.
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert_eq!(said.iter().filter(|t| t.contains("64 band pixel(s) with a value lie outside") && t.contains("/m")).count(), 1, "{said:?}");
    assert!(!said.iter().any(|t| t.contains("item(s) left out")), "{said:?}");
}

/// A mask of 32-bit whole numbers past 2^24, which `f32` rounds, is refused rather than tested on
/// rounded bits.
#[test]
fn a_mask_past_2_to_the_24_is_refused() {
    let band = |k: usize| cog((0..64).map(|c| red(k, c % 8, c / 8)).collect(), 8, None);
    let wide = || {
        rustgis_data::raster::cog::example_with(&rustgis_data::raster::cog::Example { size: 8, tile: 16, wide: true, ..Default::default() })
    };
    let (files, _) = serve_ranged_files(vec![
        ("/b0.tif".into(), band(0)),
        ("/b1.tif".into(), band(1)),
        ("/m0.tif".into(), wide()),
        ("/m1.tif".into(), wide()),
    ]);
    let features: Vec<String> = ["2024-01-01", "2024-01-17"]
        .iter()
        .enumerate()
        .map(|(k, d)| {
            format!(
                r#"{{"type": "Feature", "id": "s{k}", "collection": "s2", "bbox": [10, 45, 11, 46], "geometry": null,
                   "properties": {{"datetime": "{d}T00:00:00Z", "bdc:tiles": ["t1"]}},
                   "assets": {{"B04": {{"href": "{files}/b{k}.tif"}}, "QA": {{"href": "{files}/m{k}.tif"}}}}}}"#
            )
        })
        .collect();
    let search = format!(r#"{{"type": "FeatureCollection", "features": [{}]}}"#, features.join(","));
    let api = format!("{}/v1", serve_routes(vec![("/v1/search", search)], 8));
    let mut s = Session::new(Project::default());
    for order in ["date", "clear"] {
        let p = json!({"url": api, "collection": "s2", "asset": "B04", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-01-31",
            "mask": {"asset": "QA", "bits": {"unset": [0]}}, "stat": "first", "order": order});
        let e = s.run("cube.reduce", &p).unwrap_err();
        assert!(e.contains("past 2^24") && e.contains("/m"), "{order}: {e}");
    }
}

/// The clearest date is the one whose masks let through the most of the area, counting once
/// what two overlapping tiles both let through: date 0's two tiles are both clear in the west
/// half only (a share of 0.5, not 1), date 1's one tile in the west three quarters (0.75), so
/// date 1 comes first. Date 1's mask stores 0 in the east, a no-data its STAC metadata names but
/// its header doesn't: never let through, even by a mask that drops only the clouds.
#[test]
fn the_clear_share_counts_overlapping_tiles_once_and_skips_declared_no_data() {
    let band = |k: usize| cog((0..64).map(|c| red(k, c % 8, c / 8)).collect(), 8, None);
    let west = || cog((0..64).map(|c| if c % 8 < 4 { 4.0 } else { 9.0 }).collect(), 8, None);
    let three = cog((0..64).map(|c| if c % 8 < 6 { 4.0 } else { 0.0 }).collect(), 8, None);
    let (files, _) = serve_ranged_files(vec![
        ("/a1.tif".into(), band(0)),
        ("/a2.tif".into(), band(0)),
        ("/b.tif".into(), band(1)),
        ("/ma1.tif".into(), west()),
        ("/ma2.tif".into(), west()),
        ("/mb.tif".into(), three),
    ]);
    let item = |id: &str, d: &str, tile: &str, b: &str, m: &str, extra: &str| {
        format!(
            r#"{{"type": "Feature", "id": "{id}", "collection": "s2", "bbox": [10, 45, 11, 46], "geometry": null,
               "properties": {{"datetime": "{d}T00:00:00Z", "bdc:tiles": ["{tile}"]}},
               "assets": {{"B04": {{"href": "{files}/{b}"}}, "SCL": {{"href": "{files}/{m}"{extra}}}}}}}"#
        )
    };
    let features = [
        item("a1", "2024-01-01", "t1", "a1.tif", "ma1.tif", ""),
        item("a2", "2024-01-01", "t2", "a2.tif", "ma2.tif", ""),
        item("b", "2024-01-17", "t1", "b.tif", "mb.tif", r#", "raster:bands": [{"nodata": 0}]"#),
    ];
    let search = format!(r#"{{"type": "FeatureCollection", "features": [{}]}}"#, features.join(","));
    let api = format!("{}/v1", serve_routes(vec![("/v1/search", search)], 8));
    let mut s = Session::new(Project::default());
    let p = json!({"url": api, "collection": "s2", "asset": "B04", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-01-31",
        "mask": {"asset": "SCL", "drop": [9]}, "stat": "first"});
    let r = s.run("cube.reduce", &p).unwrap_or_else(|e| panic!("{e}"));
    let data = s.map().layer(r["layers"][0].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    for (c, v) in data.bands[0].data.iter().enumerate() {
        let (col, row) = (c % 8, c / 8);
        if col < 6 {
            assert_eq!(*v, red(1, col, row), "({col}, {row}): date 1 first");
        } else {
            assert!(v.is_nan(), "({col}, {row}): {v}, not empty");
        }
    }
}

/// Two items of one day on two tiles that share one data file each keep their own mask: the
/// first tile's clear in the west half, the second's in the east, so every cell has that day's
/// value once; the other day is all cloud.
#[test]
fn items_sharing_a_data_file_keep_their_own_masks() {
    let band = cog((0..64).map(|c| red(0, c % 8, c / 8)).collect(), 8, None);
    let half = |west: bool| cog((0..64).map(|c| if (c % 8 < 4) == west { 4.0 } else { 9.0 }).collect(), 8, None);
    let (files, _) = serve_ranged_files(vec![
        ("/b.tif".into(), band),
        ("/mw.tif".into(), half(true)),
        ("/me.tif".into(), half(false)),
        ("/mc.tif".into(), cog(vec![9.0; 64], 8, None)),
    ]);
    let item = |id: &str, d: &str, tile: &str, m: &str| {
        format!(
            r#"{{"type": "Feature", "id": "{id}", "collection": "s2", "bbox": [10, 45, 11, 46], "geometry": null,
               "properties": {{"datetime": "{d}T00:00:00Z", "bdc:tiles": ["{tile}"]}},
               "assets": {{"B04": {{"href": "{files}/b.tif"}}, "SCL": {{"href": "{files}/{m}"}}}}}}"#
        )
    };
    let features =
        [item("w", "2024-01-01", "t1", "mw.tif"), item("e", "2024-01-01", "t2", "me.tif"), item("c", "2024-01-17", "t1", "mc.tif")];
    let search = format!(r#"{{"type": "FeatureCollection", "features": [{}]}}"#, features.join(","));
    let api = format!("{}/v1", serve_routes(vec![("/v1/search", search)], 8));
    let mut s = Session::new(Project::default());
    let p = json!({"url": api, "collection": "s2", "asset": "B04", "bbox": [10, 45, 11, 46], "start": "2024-01-01", "end": "2024-01-31",
        "mask": {"asset": "SCL", "keep": [4]}, "stat": "count"});
    let r = s.run("cube.reduce", &p).unwrap_or_else(|e| panic!("{e}"));
    let data = s.map().layer(r["layers"][0].as_u64().unwrap() as u32).unwrap().raster.as_ref().unwrap().data.clone().unwrap();
    assert!(data.bands[0].data.iter().all(|v| *v == 1.0), "{:?}", data.bands[0].data);
}
