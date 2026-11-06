//! GeoTIFF reading and writing, including tiled and cloud-optimised files.
//!
//! Supports grey, RGB(A), palette and multiband images of any integer or floating-point pixel
//! type, stored in strips or tiles, chunky or planar, uncompressed or compressed with Deflate, LZW,
//! PackBits or JPEG. Georeferencing comes from the GeoTIFF tags (pixel scale and tie point, or a
//! transformation matrix) and the EPSG code in the GeoKey directory. A rotated or skewed image is
//! resampled to a north-up grid as it's read.

use std::io::{Read, Seek};

use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

use super::cog::Sparse;
use super::crs::CustomCrs;
use super::{ColorHint, Crs, Exact, GeoTransform, Raster, Uncounted, max_samples};
use crate::{DataError, Result};

fn err(e: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("GeoTIFF: {e}"))
}

/// The GeoTIFF parts of a TIFF's first image.
struct Geo {
    transform: GeoTransform,
    /// A rotated or skewed image's full transformation (GDAL's order: x, x per column, x per row,
    /// y, y per column, y per row); `transform` then holds only its origin and the column and row
    /// steps along the axes.
    affine: Option<[f64; 6]>,
    crs: Option<Crs>,
    /// An EPSG code that was named but isn't known.
    unknown_epsg: Option<u32>,
    geographic_model: bool,
    nodata: Option<f64>,
    /// The no-data value exactly as the tag writes it (a whole number past 2^53 stays whole).
    nodata_exact: Option<Exact>,
    /// Why the file's own projection couldn't be read, if it has one.
    custom_error: Option<String>,
    /// `false` for a TIFF with no GeoTIFF tags at all: `transform` is then a stand-in.
    referenced: bool,
}

/// How `n` samples of a grey image the decoder calls multiband are shown: grey with alpha when
/// there are two and the second is declared alpha (extra sample 1, associated, or 2, unassociated),
/// else grey. GDAL marks the second band of an ordinary two-band file as 0, unspecified data.
pub(super) fn multiband_color<R: Read + Seek>(dec: &mut Decoder<R>, n: u16) -> ColorHint {
    let alpha =
        n == 2 && dec.find_tag_unsigned_vec::<u16>(Tag::ExtraSamples).ok().flatten().is_some_and(|v| matches!(v.first(), Some(1 | 2)));
    if alpha { ColorHint::GrayAlpha } else { ColorHint::Gray }
}

fn geo_tags<R: Read + Seek>(dec: &mut Decoder<R>) -> Result<Geo> {
    let doubles =
        |dec: &mut Decoder<R>, tag: u16| -> Option<Vec<f64>> { dec.find_tag(Tag::Unknown(tag)).ok().flatten()?.into_f64_vec().ok() };
    // GeoKey directory: a header of four shorts, then (key, location, count, value) entries.
    let keys: Vec<u16> = dec.find_tag(Tag::GeoKeyDirectoryTag).ok().flatten().and_then(|v| v.into_u16_vec().ok()).unwrap_or_default();
    let key = |k: u16| -> Option<u16> {
        keys.get(4..)?.as_chunks::<4>().0.iter().find(|[key, location, _, _]| *key == k && *location == 0).map(|[_, _, _, value]| *value)
    };
    let pixel_is_point = key(1025) == Some(2);
    let geographic_model = key(1024) == Some(2);
    let usable = |c: &u16| *c != 32767 && *c != 0;
    let gdoubles = doubles(dec, 34736).unwrap_or_default();
    let gascii = dec.find_tag(Tag::Unknown(34737)).ok().flatten().and_then(|v| v.into_string().ok()).unwrap_or_default();
    // A projected file with a user-defined projection (the NLCD's Albers, say) defines it in its
    // keys: read as its own system, never as the geographic system it's based on.
    let user_defined = key(1024) == Some(1) && key(3072).is_none_or(|c| !usable(&c));
    // A projection the keys don't describe well enough is left unknown (the open goes on, and fails
    // later only if the coordinates aren't degrees), its reason kept for the message.
    let mut custom_error = None;
    let (code, crs) = if user_defined {
        match custom_crs(&keys, &gdoubles, &gascii) {
            Ok(crs) => (None, Some(crs)),
            Err(e) => {
                custom_error = Some(e.to_string());
                (None, None)
            }
        }
    } else {
        let code = key(3072).filter(usable).or(key(2048).filter(usable)).map(u32::from);
        (code, code.and_then(Crs::from_epsg))
    };

    // A TIFF with none of the GeoTIFF tags is a scan; one with only some of them is damaged.
    let tagless = keys.is_empty() && [33550u16, 33922, 34264].iter().all(|t| dec.find_tag(Tag::Unknown(*t)).ok().flatten().is_none());
    let mut referenced = true;
    let mut affine = None;
    let transform = if let Some(m) = doubles(dec, 34264).filter(|m| m.len() >= 8) {
        let at = |i: usize| m.get(i).copied().unwrap_or(0.0);
        if at(1) != 0.0 || at(4) != 0.0 {
            affine = Some([at(3), at(0), at(1), at(7), at(4), at(5)]);
        }
        GeoTransform { origin_x: at(3), origin_y: at(7), pixel_width: at(0), pixel_height: at(5) }
    } else {
        match (doubles(dec, 33550).filter(|s| s.len() >= 2), doubles(dec, 33922).filter(|t| t.len() >= 6)) {
            (Some(scale), Some(tie)) => {
                let s = |i: usize| scale.get(i).copied().unwrap_or(0.0);
                let t = |i: usize| tie.get(i).copied().unwrap_or(0.0);
                GeoTransform { origin_x: t(3) - t(0) * s(0), origin_y: t(4) + t(1) * s(1), pixel_width: s(0), pixel_height: -s(1) }
            }
            _ if tagless => {
                referenced = false;
                GeoTransform { origin_x: 0.0, origin_y: 0.0, pixel_width: 1.0, pixel_height: -1.0 }
            }
            _ => return Err(err("the file isn't georeferenced")),
        }
    };
    // A point's value is at a pixel's centre: the image starts half a pixel up and left of it.
    let affine =
        affine.map(
            |[x, xc, xr, y, yc, yr]| {
                if pixel_is_point { [x - (xc + xr) / 2.0, xc, xr, y - (yc + yr) / 2.0, yc, yr] } else { [x, xc, xr, y, yc, yr] }
            },
        );
    let transform = if pixel_is_point {
        GeoTransform {
            origin_x: transform.origin_x - transform.pixel_width / 2.0,
            origin_y: transform.origin_y - transform.pixel_height / 2.0,
            ..transform
        }
    } else {
        transform
    };
    let nodata_exact = dec
        .find_tag(Tag::GdalNodata)
        .ok()
        .flatten()
        .and_then(|v| v.into_string().ok())
        .and_then(|s| Exact::parse(s.trim_matches(|c: char| c == '\0' || c.is_whitespace())));
    let nodata = nodata_exact.map(Exact::wide);
    Ok(Geo {
        transform,
        affine,
        crs,
        unknown_epsg: code.filter(|_| crs.is_none()),
        geographic_model,
        nodata,
        nodata_exact,
        custom_error,
        referenced,
    })
}

/// A cloud-optimised GeoTIFF's coordinate system, georeferencing and no-data value, from its first
/// image (as [`read`] finds them); `None` for a rotated or skewed image, or one with no GeoTIFF
/// tags (a scan), which is read whole.
pub(crate) fn cog_geo<R: Read + Seek>(dec: &mut Decoder<R>) -> Result<Option<(Crs, GeoTransform, Option<Exact>)>> {
    let geo = geo_tags(dec)?;
    if geo.affine.is_some() || !geo.referenced {
        return Ok(None);
    }
    let crs = match geo.crs {
        Some(c) => c,
        None if geo.geographic_model => Crs::LonLat,
        None => match geo.unknown_epsg {
            Some(code) => return Err(err(super::crs::unusable_epsg(code))),
            None => {
                return Err(match geo.custom_error {
                    Some(e) => DataError::Format(e),
                    None => err("the file's coordinate system isn't recognised"),
                });
            }
        },
    };
    Ok(Some((crs, geo.transform, geo.nodata_exact)))
}

#[cfg(test)]
pub(crate) mod tests_support {
    /// A small GeoTIFF in strips (as [`super::write`] writes them).
    pub(crate) fn strip_tiff() -> Vec<u8> {
        let r = crate::raster::Raster::new(
            "s",
            4,
            4,
            vec![vec![1.0; 16]],
            crate::raster::GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 0.25, pixel_height: -0.25 },
            crate::raster::Crs::LonLat,
            None,
            crate::raster::ColorHint::Gray,
            "Float32",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        super::write(&r, 0).unwrap_or_else(|e| panic!("{e}"))
    }

    /// A 20×10 byte image in UTM zone 33N placed by a transformation matrix (`m`, row by row), as
    /// GDAL writes it: 100 in its top half and 200 below, 7 at its first pixel and 9 at its last.
    pub(crate) fn matrix_tiff(m: [f64; 16], pixel_is_point: bool) -> Vec<u8> {
        use tiff::encoder::{TiffEncoder, colortype};
        use tiff::tags::Tag;
        let mut px = vec![100u8; 100];
        px.extend([200u8; 100]);
        px[0] = 7;
        px[199] = 9;
        let keys: [u16; 16] = [1, 1, 0, 3, 1024, 0, 1, 1, 1025, 0, 1, if pixel_is_point { 2 } else { 1 }, 3072, 0, 1, 32633];
        let mut out = std::io::Cursor::new(Vec::new());
        let mut enc = TiffEncoder::new(&mut out).unwrap_or_else(|e| panic!("{e}"));
        let mut img = enc.new_image::<colortype::Gray8>(20, 10).unwrap_or_else(|e| panic!("{e}"));
        img.encoder().write_tag(Tag::Unknown(34264), &m[..]).unwrap_or_else(|e| panic!("{e}"));
        img.encoder().write_tag(Tag::GeoKeyDirectoryTag, &keys[..]).unwrap_or_else(|e| panic!("{e}"));
        img.write_data(&px).unwrap_or_else(|e| panic!("{e}"));
        out.into_inner()
    }
}

/// A GeoKey's parameter: inline, or from the double or ASCII parameter tags.
enum KeyValue {
    Short(u16),
    Double(f64),
    Ascii(String),
}

fn geokey(keys: &[u16], doubles: &[f64], ascii: &str, k: u16) -> Option<KeyValue> {
    let [_, location, count, value] = *keys.get(4..)?.as_chunks::<4>().0.iter().find(|e| e[0] == k)?;
    match location {
        0 => Some(KeyValue::Short(value)),
        34736 => doubles.get(value as usize).copied().map(KeyValue::Double),
        34737 => ascii
            .get(value as usize..(value as usize).saturating_add(count as usize))
            .map(|s| KeyValue::Ascii(s.trim_end_matches(['|', '\0']).to_string())),
        _ => None,
    }
}

/// A user-defined projection from a file's GeoKeys, as a PROJ definition.
fn custom_crs(keys: &[u16], doubles: &[f64], ascii: &str) -> Result<Crs> {
    let (proj4, label) = proj4_of_keys(keys, doubles, ascii)?;
    Crs::custom(CustomCrs { proj4, label, geokeys: keys.to_vec(), doubles: doubles.to_vec(), ascii: ascii.to_string() }).map_err(err)
}

/// A user-defined projection's PROJ definition and name, from its GeoKeys.
fn proj4_of_keys(keys: &[u16], doubles: &[f64], ascii: &str) -> Result<(String, String)> {
    let short = |k| match geokey(keys, doubles, ascii, k) {
        Some(KeyValue::Short(v)) => Some(v),
        _ => None,
    };
    let num = |k| match geokey(keys, doubles, ascii, k) {
        Some(KeyValue::Double(v)) if v.is_finite() => Some(v),
        _ => None,
    };
    let text = |k| match geokey(keys, doubles, ascii, k) {
        Some(KeyValue::Ascii(s)) => Some(s),
        _ => None,
    };
    // The first of several keys a parameter may be given by.
    let any = |ks: &[u16]| ks.iter().find_map(|k| num(*k));
    // Metres per linear unit: false eastings and northings are in the file's units, PROJ's in
    // metres.
    let (to_m, units) = match short(3076) {
        Some(9002) => (0.3048, "+units=ft".to_string()),
        Some(9003) => (1200.0 / 3937.0, "+units=us-ft".to_string()),
        _ => match num(3077) {
            Some(m) if m > 0.0 && (m - 1.0).abs() > 1e-12 => (m, format!("+to_meter={m}")),
            _ => (1.0, "+units=m".to_string()),
        },
    };
    let lat0 = any(&[3081, 3085, 3089]).unwrap_or(0.0);
    let lon0 = any(&[3080, 3084, 3088, 3095]).unwrap_or(0.0);
    let x0 = any(&[3082, 3086, 3090]).unwrap_or(0.0) * to_m;
    let y0 = any(&[3083, 3087, 3091]).unwrap_or(0.0) * to_m;
    let k0 = any(&[3092, 3093]).unwrap_or(1.0);
    let (lat1, lat2) = (num(3078).unwrap_or(lat0), num(3079).unwrap_or(lat0));
    let proj = match short(3075) {
        Some(1) => format!("+proj=tmerc +lat_0={lat0} +lon_0={lon0} +k_0={k0} +x_0={x0} +y_0={y0}"),
        Some(7) => format!("+proj=merc +lon_0={lon0} +k_0={k0} +x_0={x0} +y_0={y0}"),
        Some(8) => format!("+proj=lcc +lat_1={lat1} +lat_2={lat2} +lat_0={lat0} +lon_0={lon0} +x_0={x0} +y_0={y0}"),
        Some(9) => format!("+proj=lcc +lat_1={lat0} +lat_0={lat0} +lon_0={lon0} +k_0={k0} +x_0={x0} +y_0={y0}"),
        Some(10) => format!("+proj=laea +lat_0={lat0} +lon_0={lon0} +x_0={x0} +y_0={y0}"),
        Some(11) => format!("+proj=aea +lat_1={lat1} +lat_2={lat2} +lat_0={lat0} +lon_0={lon0} +x_0={x0} +y_0={y0}"),
        Some(12) => format!("+proj=aeqd +lat_0={lat0} +lon_0={lon0} +x_0={x0} +y_0={y0}"),
        Some(14) => format!("+proj=stere +lat_0={lat0} +lon_0={lon0} +k_0={k0} +x_0={x0} +y_0={y0}"),
        Some(15) => {
            // Polar: the pole on the side of the latitude of true scale.
            let pole = if lat0 < 0.0 { -90.0 } else { 90.0 };
            format!("+proj=stere +lat_0={pole} +lat_ts={lat0} +lon_0={lon0} +k_0={k0} +x_0={x0} +y_0={y0}")
        }
        Some(16) => format!("+proj=sterea +lat_0={lat0} +lon_0={lon0} +k_0={k0} +x_0={x0} +y_0={y0}"),
        Some(17) => format!("+proj=eqc +lat_ts={lat1} +lon_0={lon0} +x_0={x0} +y_0={y0}"),
        // MODIS's grid; RustGIS computes it (`rustgis_geom::Sinusoidal`).
        Some(24) => format!("+proj=sinu +lon_0={lon0} +x_0={x0} +y_0={y0}"),
        Some(28) => format!("+proj=cea +lat_ts={lat1} +lon_0={lon0} +x_0={x0} +y_0={y0}"),
        Some(c) => return Err(err(format!("the file's own projection (coordinate transformation {c}) isn't supported"))),
        None => return Err(err("the file's own projection isn't described in its keys")),
    };
    // The datum or ellipsoid, by code or by its axes.
    let earth = match (short(2048), short(2050), short(2056)) {
        (Some(4326), _, _) | (_, Some(6326), _) | (_, _, Some(7030)) => "+datum=WGS84".to_string(),
        (Some(4269), _, _) | (_, Some(6269), _) | (_, _, Some(7019)) => "+ellps=GRS80 +towgs84=0,0,0".to_string(),
        (Some(4267), _, _) | (_, Some(6267), _) | (_, _, Some(7008)) => "+ellps=clrk66".to_string(),
        _ => match (num(2057), num(2059), num(2058)) {
            (Some(a), Some(rf), _) if a > 0.0 && rf > 0.0 => format!("+a={a} +rf={rf}"),
            (Some(a), _, Some(b)) if a > 0.0 && b > 0.0 => format!("+a={a} +b={b}"),
            _ => "+datum=WGS84".to_string(),
        },
    };
    let label = text(3073).or_else(|| text(1026)).filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "Custom projection".into());
    Ok((format!("{proj} {earth} {units} +no_defs"), label))
}

/// Whether two PROJ definitions place the same points: points around the projection's origin,
/// unprojected by both, land within a millimetre-scale tolerance.
fn same_projection(a: &str, b: &str) -> bool {
    let offsets = [(0.0, 0.0), (100_000.0, 50_000.0), (-250_000.0, 150_000.0), (300_000.0, -200_000.0), (-50_000.0, -400_000.0)];
    // Sinusoidal, which proj4rs lacks, by its own inverse; one sinusoidal and one not differ.
    match (rustgis_geom::Sinusoidal::from_proj4(a), rustgis_geom::Sinusoidal::from_proj4(b)) {
        (Some(Ok(sa)), Some(Ok(sb))) => {
            return offsets.iter().all(|(dx, dy)| {
                // The same map point in both, so a different false origin is a different projection.
                let m = rustgis_geom::Coord::new(sa.x0 + dx, sa.y0 + dy);
                // Degrees: 1e-7 is about a centimetre.
                matches!((sa.inverse(m), sb.inverse(m)), (Some(p), Some(q)) if (p.x - q.x).abs() < 1e-7 && (p.y - q.y).abs() < 1e-7)
            });
        }
        (None, None) => {}
        _ => return false,
    }
    let (Ok(pa), Ok(pb), Ok(ll)) = (
        proj4rs::Proj::from_proj_string(a),
        proj4rs::Proj::from_proj_string(b),
        proj4rs::Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs"),
    ) else {
        return false;
    };
    offsets.iter().all(|(dx, dy)| {
        let (mut p, mut q) = ((*dx, *dy, 0.0), (*dx, *dy, 0.0));
        let ok = proj4rs::transform::transform(&pa, &ll, &mut p).is_ok() && proj4rs::transform::transform(&pb, &ll, &mut q).is_ok();
        // Radians: 1e-9 is under a centimetre.
        ok && p.0.is_finite() && p.1.is_finite() && (p.0 - q.0).abs() < 1e-9 && (p.1 - q.1).abs() < 1e-9
    })
}

/// GeoKeys for a user-defined projection from its PROJ definition (one read from WKT, which
/// came with no keys), the inverse of [`custom_crs`]: the key directory, its double parameters and
/// its ASCII parameters. `None` for a projection the keys can't say.
fn keys_from_proj4(proj4: &str, label: &str) -> Option<(Vec<u16>, Vec<f64>, String)> {
    let params: Vec<(&str, &str)> = proj4.split_whitespace().take(64).filter_map(|t| t.trim_start_matches('+').split_once('=')).collect();
    let get = |k: &str| params.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
    // The keys are written in metres: a definition in other units isn't written.
    if get("units").is_some_and(|u| u != "m") || get("to_meter").is_some() {
        return None;
    }
    // Nor one with a parameter they can't say (a datum shift, a prime meridian, an axis order,
    // grids): it would be written as another projection.
    const SAID: [&str; 16] =
        ["proj", "lat_0", "lon_0", "lat_1", "lat_2", "lat_ts", "x_0", "y_0", "k_0", "k", "datum", "ellps", "a", "b", "R", "rf"];
    let tokens = proj4.split_whitespace().map(|t| t.trim_start_matches('+'));
    if tokens.map(|t| t.split('=').next().unwrap_or(t)).any(|k| !SAID.contains(&k) && !matches!(k, "units" | "no_defs" | "type" | "wktext"))
    {
        return None;
    }
    // A latitude of true scale is said only for the projections that take one.
    // A scale factor only for the projections whose keys carry one.
    let scaled = get("k_0").or(get("k")).and_then(|v| v.parse::<f64>().ok()).is_some_and(|k| (k - 1.0).abs() > 1e-12);
    if scaled && !matches!(get("proj"), Some("tmerc" | "merc" | "lcc" | "stere" | "sterea")) {
        return None;
    }
    let polar = get("lat_0").and_then(|v| v.parse::<f64>().ok()).is_some_and(|l| l.abs() == 90.0);
    if get("lat_ts").is_some() && !(matches!(get("proj"), Some("eqc" | "cea")) || (get("proj") == Some("stere") && polar)) {
        return None;
    }
    let num = |k: &str| get(k).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite());
    let (lat0, lon0) = (num("lat_0").unwrap_or(0.0), num("lon_0").unwrap_or(0.0));
    let (x0, y0, k0) = (num("x_0").unwrap_or(0.0), num("y_0").unwrap_or(0.0), num("k_0").or(num("k")).unwrap_or(1.0));
    // The transformation, and its parameters by key (in metres and degrees).
    let (ct, mut doubles_by_key): (u16, Vec<(u16, f64)>) = match get("proj")? {
        "tmerc" => (1, vec![(3080, lon0), (3081, lat0), (3082, x0), (3083, y0), (3092, k0)]),
        "merc" => (7, vec![(3080, lon0), (3082, x0), (3083, y0), (3092, k0)]),
        // Two standard parallels (CT 8) carry no scale factor: one that isn't 1 is said by the
        // one-parallel form (CT 9), when the parallels are one, or not at all.
        "lcc" => {
            let lat1 = num("lat_1").unwrap_or(lat0);
            match num("lat_2") {
                Some(lat2) if (k0 - 1.0).abs() < 1e-12 => {
                    (8, vec![(3078, lat1), (3079, lat2), (3084, lon0), (3085, lat0), (3086, x0), (3087, y0)])
                }
                Some(lat2) if (lat2 - lat1).abs() > 1e-12 => return None,
                // The one-parallel form's latitude is both the parallel and the origin's.
                _ if (lat1 - lat0).abs() > 1e-12 => return None,
                _ => (9, vec![(3080, lon0), (3081, lat1), (3082, x0), (3083, y0), (3092, k0)]),
            }
        }
        "laea" => (10, vec![(3080, lon0), (3081, lat0), (3082, x0), (3083, y0)]),
        "aea" => (
            11,
            vec![
                (3078, num("lat_1").unwrap_or(lat0)),
                (3079, num("lat_2").unwrap_or(lat0)),
                (3084, lon0),
                (3085, lat0),
                (3086, x0),
                (3087, y0),
            ],
        ),
        "aeqd" => (12, vec![(3080, lon0), (3081, lat0), (3082, x0), (3083, y0)]),
        // Polar: the latitude of true scale is the key's latitude.
        // Its longitude is the straight vertical pole's (3095), as GDAL writes it.
        "stere" if lat0.abs() == 90.0 => {
            (15, vec![(3081, num("lat_ts").unwrap_or(lat0)), (3082, x0), (3083, y0), (3092, k0), (3095, lon0)])
        }
        "stere" => (14, vec![(3080, lon0), (3081, lat0), (3082, x0), (3083, y0), (3092, k0)]),
        "sterea" => (16, vec![(3080, lon0), (3081, lat0), (3082, x0), (3083, y0), (3092, k0)]),
        "eqc" => (17, vec![(3078, num("lat_ts").unwrap_or(0.0)), (3080, lon0), (3082, x0), (3083, y0)]),
        "cea" => (28, vec![(3078, num("lat_ts").unwrap_or(0.0)), (3080, lon0), (3082, x0), (3083, y0)]),
        // Its longitude is the projection centre's (3088), as GDAL and MODIS write it.
        "sinu" => (24, vec![(3082, x0), (3083, y0), (3088, lon0)]),
        _ => return None,
    };
    // The earth: WGS 84 by code, any other by its axes.
    let mut shorts: Vec<(u16, u16)> = vec![(1024, 1), (1025, 1), (3072, 32767), (3074, 32767), (3075, ct), (3076, 9001)];
    if get("datum") == Some("WGS84") || (get("ellps") == Some("WGS84") && get("a").is_none()) {
        shorts.push((2048, 4326));
    } else {
        let (a, b) = match (num("a").or(num("R")), num("b"), num("rf")) {
            (Some(a), Some(b), _) => (a, b),
            (Some(a), None, Some(rf)) if rf > 0.0 => (a, a - a / rf),
            (Some(r), None, _) => (r, r),
            _ => return None,
        };
        shorts.extend([(2048, 32767), (2050, 32767), (2056, 32767)]);
        doubles_by_key.extend([(2057, a), (2058, b)]);
    }
    // The key directory: sorted by key, doubles and ASCII at their offsets.
    // TIFF's ASCII is 7-bit.
    let label: String = label.chars().filter(|c| c.is_ascii() && !c.is_ascii_control() && *c != '|').take(200).collect();
    let mut entries: Vec<[u16; 4]> = shorts.iter().map(|(k, v)| [*k, 0, 1, *v]).collect();
    doubles_by_key.sort_by_key(|(k, _)| *k);
    let doubles: Vec<f64> = doubles_by_key.iter().map(|(_, v)| *v).collect();
    for (i, (k, _)) in doubles_by_key.iter().enumerate() {
        entries.push([*k, 34736, 1, i as u16]);
    }
    let ascii = format!("{label}|");
    entries.push([3073, 34737, ascii.len() as u16, 0]);
    entries.sort_by_key(|e| e[0]);
    let mut keys = vec![1, 1, 0, entries.len() as u16];
    keys.extend(entries.iter().flatten());
    // The keys must say the same projection: read back, it places points where the definition
    // does (anything they can't carry, written differently, is refused rather than shifted).
    let (back, _) = proj4_of_keys(&keys, &doubles, &ascii).ok()?;
    same_projection(proj4, &back).then_some((keys, doubles, ascii))
}

/// The samples as `f32`, with their type's name. Their memory is reserved first: when it can't be
/// had, the error names the `w × h` raster being made (a decimated read's, smaller than the file).
fn to_f32(buf: DecodingResult, (w, h): (usize, usize)) -> Result<(Vec<f32>, &'static str)> {
    fn held<T>(v: Vec<T>, f: impl Fn(T) -> f32, (w, h): (usize, usize)) -> Result<Vec<f32>> {
        let mut out = super::room(v.len(), w, h)?;
        out.extend(v.into_iter().map(f));
        Ok(out)
    }
    let at = (w, h);
    Ok(match buf {
        DecodingResult::U8(v) => (held(v, f32::from, at)?, "UInt8"),
        DecodingResult::I8(v) => (held(v, f32::from, at)?, "Int8"),
        DecodingResult::U16(v) => (held(v, f32::from, at)?, "UInt16"),
        DecodingResult::I16(v) => (held(v, f32::from, at)?, "Int16"),
        DecodingResult::U32(v) => (held(v, |x| x as f32, at)?, "UInt32"),
        DecodingResult::I32(v) => (held(v, |x| x as f32, at)?, "Int32"),
        DecodingResult::U64(v) => (held(v, |x| x as f32, at)?, "UInt64"),
        DecodingResult::I64(v) => (held(v, |x| x as f32, at)?, "Int64"),
        DecodingResult::F16(v) => (held(v, f32::from, at)?, "Float16"),
        DecodingResult::F32(v) => (v, "Float32"),
        DecodingResult::F64(v) => (held(v, |x| x as f32, at)?, "Float64"),
    })
}

/// How many samples were decoded.
fn sample_count(buf: &DecodingResult) -> usize {
    match buf {
        DecodingResult::U8(v) => v.len(),
        DecodingResult::I8(v) => v.len(),
        DecodingResult::U16(v) => v.len(),
        DecodingResult::I16(v) => v.len(),
        DecodingResult::U32(v) => v.len(),
        DecodingResult::I32(v) => v.len(),
        DecodingResult::U64(v) => v.len(),
        DecodingResult::I64(v) => v.len(),
        DecodingResult::F16(v) => v.len(),
        DecodingResult::F32(v) => v.len(),
        DecodingResult::F64(v) => v.len(),
    }
}

/// Whether some values of this pixel type can't be held exactly as `f32`: 32- and 64-bit whole
/// numbers and 64-bit floats.
fn can_round(buf: &DecodingResult) -> bool {
    matches!(
        buf,
        DecodingResult::U32(_) | DecodingResult::I32(_) | DecodingResult::U64(_) | DecodingResult::I64(_) | DecodingResult::F64(_)
    )
}

/// The samples of `buf` that `f32` doesn't hold (see [`held_exactly`]), a bit each: `None` for a type
/// that holds every value, or when none changed.
pub(crate) fn rounded_bits(buf: &DecodingResult, nodata: Uncounted) -> Option<Vec<u64>> {
    if !can_round(buf) {
        return None;
    }
    let n = sample_count(buf);
    let mut bits: Option<Vec<u64>> = None;
    for j in 0..n {
        if !held_exactly(buf, j, nodata)
            && let Some(w) = bits.get_or_insert_with(|| vec![0u64; n.div_ceil(64)]).get_mut(j / 64)
        {
            *w |= 1 << (j % 64);
        }
    }
    bits
}

/// Whether `f32` holds sample `j` of `buf` exactly, as [`to_f32`] converts it: whole numbers past
/// ±16,777,216 and 64-bit floats with more precision than `f32` are rounded. NaN, a missing sample
/// and no-data samples (by [`Uncounted`]'s rule) count as held.
fn held_exactly(buf: &DecodingResult, j: usize, nodata: Uncounted) -> bool {
    let held = |nan: bool, f: f32, exact: bool, value: Exact| nan || nodata.decides(f, || Some(value)).unwrap_or(exact);
    // Whole numbers are compared in i128, which holds every u64 and i64 and the f32 just past them.
    let whole = |f: f32, i: i128| held(false, f, f as i128 == i, Exact::Whole(i));
    match buf {
        DecodingResult::U32(v) => v.get(j).is_none_or(|&x| whole(x as f32, i128::from(x))),
        DecodingResult::I32(v) => v.get(j).is_none_or(|&x| whole(x as f32, i128::from(x))),
        DecodingResult::U64(v) => v.get(j).is_none_or(|&x| whole(x as f32, i128::from(x))),
        DecodingResult::I64(v) => v.get(j).is_none_or(|&x| whole(x as f32, i128::from(x))),
        DecodingResult::F64(v) => v.get(j).is_none_or(|&x| held(x.is_nan(), x as f32, f64::from(x as f32) == x, Exact::Float(x))),
        _ => true,
    }
}

/// Whether the current image is LZW-compressed.
fn lzw<R: Read + Seek>(dec: &mut Decoder<R>) -> bool {
    dec.find_tag_unsigned::<u16>(Tag::Compression).ok().flatten() == Some(5)
}

/// Decodes chunk `index` (a strip or tile) of the current image. The `tiff` crate fails on some
/// LZW chunks it should read (issue #350): those are decoded by [`super::lzw`] instead.
pub(crate) fn read_chunk<R: Read + Seek>(dec: &mut Decoder<R>, index: u32) -> Result<DecodingResult> {
    match dec.read_chunk(index) {
        Ok(v) => Ok(v),
        Err(e) if lzw(dec) => super::lzw::chunk(dec, index).map_err(|_| err(e)),
        Err(e) => Err(err(e)),
    }
}

/// Turns a JPEG-compressed image's three YCbCr bands into red, green and blue in place, with the
/// JFIF (full-range) coefficients JPEG-in-TIFF files use.
fn ycbcr_to_rgb(bands: &mut [Vec<f32>]) {
    let [y, cb, cr] = bands else { return };
    for ((y, cb), cr) in y.iter_mut().zip(cb.iter_mut()).zip(cr.iter_mut()) {
        let (luma, blue, red) = (*y, *cb - 128.0, *cr - 128.0);
        *y = (luma + 1.402 * red).round().clamp(0.0, 255.0);
        *cb = (luma - 0.344_136 * blue - 0.714_136 * red).round().clamp(0.0, 255.0);
        *cr = (luma + 1.772 * blue).round().clamp(0.0, 255.0);
    }
}

/// Bits per sample of the current image (the most of any sample).
fn bits<R: Read + Seek>(dec: &mut Decoder<R>) -> u16 {
    dec.find_tag_unsigned_vec::<u16>(Tag::BitsPerSample).ok().flatten().and_then(|v| v.into_iter().max()).unwrap_or(8)
}

/// Bytes per sample of the current image (from its bits per sample).
fn sample_bytes<R: Read + Seek>(dec: &mut Decoder<R>) -> usize {
    (bits(dec) as usize).div_ceil(8).max(1)
}

fn samples<R: Read + Seek>(dec: &mut Decoder<R>) -> usize {
    dec.find_tag_unsigned::<u16>(Tag::SamplesPerPixel).ok().flatten().unwrap_or(1).max(1) as usize
}

/// A reader that shows palette images as grey ones: the `tiff` crate decodes no palette images, but
/// decodes their colour indices as grey samples, and the colour table is read from the ColorMap tag
/// as it is. The only change is the PhotometricInterpretation value (3 → 1) in each image's IFD.
pub(crate) struct Unpaletted<R> {
    inner: R,
    /// Where each palette image's PhotometricInterpretation value is, and what to show there.
    patches: Vec<(u64, [u8; 2])>,
    /// Whether the first image (the full one) is a palette image.
    first_palette: bool,
    pos: u64,
}

/// Most images and IFD entries looked at for palette images.
const MAX_IFDS: usize = 64;
const MAX_ENTRIES: u64 = 4096;

/// Where the palette images' PhotometricInterpretation values are, walking the IFD chain
/// (classic and BigTIFF). A file it can't walk has none: the decoder reports what's wrong.
fn palette_patches<R: Read + Seek>(r: &mut R) -> (Vec<(u64, [u8; 2])>, bool) {
    use std::io::SeekFrom;
    let mut patches = Vec::new();
    // Whether the first image is one of them.
    let mut first = false;
    let mut walk = || -> Option<()> {
        let mut head = [0u8; 16];
        r.seek(SeekFrom::Start(0)).ok()?;
        r.read_exact(&mut head[..8]).ok()?;
        let little = match &head[..2] {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let u16_ = |b: &[u8]| -> Option<u16> {
            let a: [u8; 2] = b.get(..2)?.try_into().ok()?;
            Some(if little { u16::from_le_bytes(a) } else { u16::from_be_bytes(a) })
        };
        let u32_ = |b: &[u8]| -> Option<u32> {
            let a: [u8; 4] = b.get(..4)?.try_into().ok()?;
            Some(if little { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
        };
        let u64_ = |b: &[u8]| -> Option<u64> {
            let a: [u8; 8] = b.get(..8)?.try_into().ok()?;
            Some(if little { u64::from_le_bytes(a) } else { u64::from_be_bytes(a) })
        };
        let big = match u16_(head.get(2..)?)? {
            42 => false,
            43 => true,
            _ => return None,
        };
        let mut ifd = if big {
            r.read_exact(&mut head[8..16]).ok()?;
            u64_(head.get(8..)?)?
        } else {
            u64::from(u32_(head.get(4..)?)?)
        };
        let (count_len, entry_len, next_len) = if big { (8u64, 20u64, 8usize) } else { (2, 12, 4) };
        for k in 0..MAX_IFDS {
            if ifd == 0 {
                break;
            }
            r.seek(SeekFrom::Start(ifd)).ok()?;
            let mut c = [0u8; 8];
            r.read_exact(&mut c[..count_len as usize]).ok()?;
            let n = if big { u64_(&c)? } else { u64::from(u16_(&c)?) };
            if n > MAX_ENTRIES {
                return None;
            }
            let mut entries = vec![0u8; usize::try_from(n * entry_len).ok()?];
            r.read_exact(&mut entries).ok()?;
            for (i, e) in entries.chunks(entry_len as usize).enumerate() {
                // PhotometricInterpretation (262), a SHORT, its value inline.
                if u16_(e)? == 262 && u16_(e.get(2..)?)? == 3 {
                    let value_at = if big { 12 } else { 8 };
                    if u16_(e.get(value_at..)?)? == 3 {
                        let at = ifd + count_len + i as u64 * entry_len + value_at as u64;
                        patches.push((at, if little { [1, 0] } else { [0, 1] }));
                        first |= k == 0;
                    }
                }
            }
            let mut next = [0u8; 8];
            r.read_exact(&mut next[..next_len]).ok()?;
            ifd = if big { u64_(&next)? } else { u64::from(u32_(&next)?) };
        }
        Some(())
    };
    let _ = walk();
    (patches, first)
}

/// Any reader, boxed: the decoder is compiled once for every kind of source (a file, bytes in
/// memory, a COG's ranges), which keeps the browser build small.
pub(crate) trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// What the decoder reads from.
pub(crate) type Reader<'a> = Unpaletted<Box<dyn ReadSeek + 'a>>;

/// A reader for the decoder.
pub(crate) fn reader<'a>(r: impl Read + Seek + 'a) -> std::io::Result<Reader<'a>> {
    Unpaletted::new(Box::new(r))
}

impl<R> Unpaletted<R> {
    /// Whether the file's first image (the full one) is a palette image (PhotometricInterpretation
    /// 3), shown as grey.
    pub(crate) fn first_is_palette(&self) -> bool {
        self.first_palette
    }
}

impl<R: Read + Seek> Unpaletted<R> {
    fn new(mut inner: R) -> std::io::Result<Self> {
        let (patches, first_palette) = palette_patches(&mut inner);
        inner.seek(std::io::SeekFrom::Start(0))?;
        Ok(Unpaletted { inner, patches, first_palette, pos: 0 })
    }
}

impl<R: Read> Read for Unpaletted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        let (start, end) = (self.pos, self.pos + n as u64);
        for (at, bytes) in &self.patches {
            for (k, b) in bytes.iter().enumerate() {
                let p = at + k as u64;
                if p >= start
                    && p < end
                    && let Some(slot) = buf.get_mut((p - start) as usize)
                {
                    *slot = *b;
                }
            }
        }
        self.pos = end;
        Ok(n)
    }
}

impl<R: Seek> Seek for Unpaletted<R> {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.pos = self.inner.seek(to)?;
        Ok(self.pos)
    }
}

/// Opens another reader on the same file, for decoding its chunks on more than one thread.
pub(crate) type Opener<'a> = &'a (dyn Fn() -> std::io::Result<Reader<'static>> + Sync);

/// How an image is cut into chunks (strips or tiles), and how they're laid out.
#[derive(Clone, Copy)]
struct Chunks {
    w: u32,
    h: u32,
    /// Chunk width and height.
    cw: u32,
    ch: u32,
    /// Chunks across and down one plane.
    across: u32,
    per_plane: u32,
    planes: usize,
    /// Samples in each chunk's pixels.
    samples: usize,
    planar: bool,
}

impl Chunks {
    fn new<R: Read + Seek>(dec: &mut Decoder<R>, w: u32, h: u32, nbands: usize, planar: bool) -> Self {
        let tag = |dec: &mut Decoder<R>, t: Tag| dec.find_tag_unsigned::<u32>(t).ok().flatten();
        // Chunk sizes from the tags (the decoder's own helpers assume a sound file).
        let tiled = tag(dec, Tag::TileWidth).is_some();
        let (cw, ch) = if tiled {
            (tag(dec, Tag::TileWidth).unwrap_or(256).max(1), tag(dec, Tag::TileLength).unwrap_or(256).max(1))
        } else {
            (w.max(1), tag(dec, Tag::RowsPerStrip).unwrap_or(h).clamp(1, h.max(1)))
        };
        let (across, down) = (w.div_ceil(cw), h.div_ceil(ch));
        Chunks {
            w,
            h,
            cw,
            ch,
            across,
            per_plane: across.saturating_mul(down),
            planes: if planar { nbands } else { 1 },
            samples: if planar { 1 } else { nbands },
            planar,
        }
    }

    fn count(&self) -> u64 {
        u64::from(self.per_plane) * self.planes as u64
    }

    /// Chunk `i` (counting through every plane) as (plane, chunk index in the file), when it holds
    /// a kept row and column; a chunk holding none is skipped unread.
    fn wanted(&self, i: u64, k: u32) -> Option<(usize, u32)> {
        let plane = usize::try_from(i / u64::from(self.per_plane.max(1))).ok()?;
        let c = u32::try_from(i % u64::from(self.per_plane.max(1))).ok()?;
        let (x0, y0) = ((c % self.across) * self.cw, (c / self.across) * self.ch);
        let rows = self.ch.min(self.h.saturating_sub(y0));
        if y0.div_ceil(k) * k >= y0 + rows || x0.div_ceil(k) * k >= x0 + self.cw.min(self.w - x0) {
            return None;
        }
        let index = u32::try_from(plane).ok()?.checked_mul(self.per_plane)?.checked_add(c)?;
        Some((plane, index))
    }

    /// Copies every `k`-th pixel each way of chunk `c` of `plane` (decoded as `values`) into
    /// `bands`, each `ow` pixels wide, counting in `rounded` (by band) the pixels copied whose bit
    /// is set in `changed` (from [`rounded_bits`]).
    #[allow(clippy::too_many_arguments)]
    fn place(
        &self,
        bands: &mut [Vec<f32>],
        values: &[f32],
        changed: Option<&[u64]>,
        rounded: &mut [u64],
        plane: usize,
        c: u32,
        k: u32,
        ow: usize,
    ) {
        let c = c % self.per_plane.max(1);
        let (x0, y0) = ((c % self.across) * self.cw, (c / self.across) * self.ch);
        let rows = self.ch.min(self.h.saturating_sub(y0));
        // The chunk's row length: the data width, or the full tile width when padded. The tiff
        // crate gives a bottom tile of every band after the first, when the bands are stored
        // apart, its padding rows too (issue #544), so the row count can't tell it alone. Where two
        // shapes have the same size, the data width wins: both the tiff crate and our LZW decoder
        // return chunks as wide as the data.
        let per_pixel = self.samples.max(1);
        let dw = self.cw.min(self.w.saturating_sub(x0)) as usize;
        let shapes = [(dw, rows), (dw, self.ch), (self.cw as usize, rows), (self.cw as usize, self.ch)];
        let stride = shapes
            .iter()
            .find(|(cols, r)| cols.saturating_mul(*r as usize).saturating_mul(per_pixel) == values.len())
            .map_or(values.len() / (rows as usize).max(1) / per_pixel, |(cols, _)| *cols)
            .max(1);
        let mut y = y0.div_ceil(k) * k;
        while y < y0 + rows {
            let mut x = x0.div_ceil(k) * k;
            while x < (x0 + self.cw).min(self.w) {
                let src = ((y - y0) as usize * stride + (x - x0) as usize) * self.samples;
                let dst = (y / k) as usize * ow + (x / k) as usize;
                for b in 0..self.samples {
                    let band = if self.planar { plane } else { b };
                    if let (Some(v), Some(slot)) = (values.get(src + b), bands.get_mut(band).and_then(|v| v.get_mut(dst))) {
                        *slot = *v;
                        let at = src + b;
                        if let Some(bits) = changed
                            && bits.get(at / 64).is_some_and(|w| w >> (at % 64) & 1 == 1)
                            && let Some(n) = rounded.get_mut(band)
                        {
                            *n += 1;
                        }
                    }
                }
                x += k;
            }
            y += k;
        }
    }
}

/// Fewest chunks worth decoding on more than one thread.
#[cfg(not(target_arch = "wasm32"))]
const PARALLEL_CHUNKS: u64 = 16;

/// Reads an image a strip or tile at a time, keeping every `k`-th pixel each way: an overview
/// made as it is read, for a file too large to hold that has none. Returns the bands. With `k`
/// 1, the whole image a chunk at a time. With `open`, chunks are decoded on several threads,
/// each reading image `image` of its own copy of the file.
#[allow(clippy::too_many_arguments)]
fn decimate<R: Read + Seek>(
    dec: &mut Decoder<R>,
    w: u32,
    h: u32,
    nbands: usize,
    planar: bool,
    k: u32,
    nodata: Uncounted,
    open: Option<(Opener<'_>, usize, Limits)>,
) -> Result<(Vec<Vec<f32>>, &'static str, Vec<u64>)> {
    let k = k.max(1);
    let (ow, oh) = (w.div_ceil(k) as usize, h.div_ceil(k) as usize);
    let mut bands = (0..nbands).map(|_| super::filled(ow, oh, f32::NAN)).collect::<Result<Vec<_>>>()?;
    let chunks = Chunks::new(dec, w, h, nbands, planar);
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((open, image, limits)) = open
        && chunks.count() >= PARALLEL_CHUNKS
    {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(16);
        if threads > 1 {
            return decimate_parallel(chunks, bands, k, (ow, oh), nodata, threads, open, image, limits);
        }
    }
    let _ = open;
    let mut data_type = "Float32";
    let mut rounded = vec![0u64; nbands];
    for i in 0..chunks.count() {
        let Some((plane, index)) = chunks.wanted(i, k) else { continue };
        let buf = read_chunk(dec, index)?;
        let changed = rounded_bits(&buf, nodata);
        let (values, dt) = to_f32(buf, (ow, oh))?;
        data_type = dt;
        chunks.place(&mut bands, &values, changed.as_deref(), &mut rounded, plane, index, k, ow);
    }
    Ok((bands, data_type, rounded))
}

/// [`decimate`] on `threads` threads, each with its own decoder taking the next chunk in turn.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn decimate_parallel(
    chunks: Chunks,
    bands: Vec<Vec<f32>>,
    k: u32,
    (ow, oh): (usize, usize),
    nodata: Uncounted,
    threads: usize,
    open: Opener<'_>,
    image: usize,
    limits: Limits,
) -> Result<(Vec<Vec<f32>>, &'static str, Vec<u64>)> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let next = AtomicU64::new(0);
    let failed = AtomicBool::new(false);
    let failure: Mutex<Option<DataError>> = Mutex::new(None);
    let rounded = vec![0u64; bands.len()];
    let out = Mutex::new((bands, "Float32", rounded));
    let fail = |e: DataError| {
        failed.store(true, Ordering::Relaxed);
        if let Ok(mut f) = failure.lock() {
            f.get_or_insert(e);
        }
    };
    let work = || {
        let dec = open().map_err(err).and_then(|r| Decoder::new(r).map_err(err)).map(|d| d.with_limits(limits.clone()));
        let mut dec = match dec.and_then(|mut d| d.seek_to_image(image).map_err(err).map(|()| d)) {
            Ok(d) => d,
            Err(e) => return fail(e),
        };
        while !failed.load(Ordering::Relaxed) {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= chunks.count() {
                break;
            }
            let Some((plane, index)) = chunks.wanted(i, k) else { continue };
            match read_chunk(&mut dec, index) {
                Ok(buf) => {
                    let changed = rounded_bits(&buf, nodata);
                    let (values, dt) = match to_f32(buf, (ow, oh)) {
                        Ok(v) => v,
                        Err(e) => return fail(e),
                    };
                    if let Ok(mut o) = out.lock() {
                        let (bands, data_type, rounded) = &mut *o;
                        *data_type = dt;
                        chunks.place(bands, &values, changed.as_deref(), rounded, plane, index, k, ow);
                    }
                }
                Err(e) => return fail(e),
            }
        }
    };
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads).map(|_| scope.spawn(work)).collect();
        for h in handles {
            if h.join().is_err() {
                fail(err("a chunk couldn't be decoded"));
            }
        }
    });
    if let Some(e) = failure.into_inner().ok().flatten() {
        return Err(e);
    }
    out.into_inner().map_err(|_| err("a chunk couldn't be decoded"))
}

/// Reads a GeoTIFF. Images too large for [`max_samples`] are read from their largest overview that
/// fits, when the file has overviews; otherwise an overview is made as the image is read, a strip
/// or tile at a time (a regular, not cloud-optimised, file).
pub fn read<R: Read + Seek>(r: R, name: &str) -> Result<Raster> {
    read_with_nodata(r, name, None)
}

/// [`read`], with the no-data value a sidecar gives (see [`read_limited_with_nodata`]).
pub(super) fn read_with_nodata<R: Read + Seek>(r: R, name: &str, sidecar_nodata: Option<f64>) -> Result<Raster> {
    read_limited_with_nodata(reader(r).map_err(err)?, name, max_samples(), None, sidecar_nodata)
}

/// What reading a GeoTIFF from the parts of it read so far came to.
#[derive(Debug)]
pub enum Partial {
    Done(Box<Raster>),
    /// These bytes (first, end) must be read first.
    Need(u64, u64),
}

/// Reads a GeoTIFF from the parts of it read so far, as [`read`] does: the raster, or the range
/// to read next. The web app reads a large file so, a part at a time, keeping only its header and
/// the overview it opens from (or the strips or tiles an overview made as it is read needs).
pub fn read_sparse(parts: &Sparse, name: &str) -> Result<Partial> {
    read_sparse_limited(parts, name, max_samples())
}

/// [`read_sparse`] of the parts the web app saved, with its limit ([`super::BROWSER_SAMPLES`]): it
/// kept the overview it chose with that, which a larger limit could pass over for a finer one the
/// parts don't hold.
pub fn read_saved_parts(parts: &Sparse, name: &str) -> Result<Partial> {
    read_sparse_limited(parts, name, super::BROWSER_SAMPLES)
}

fn read_sparse_limited(parts: &Sparse, name: &str, max: usize) -> Result<Partial> {
    let r = parts.reader();
    let watch = r.clone();
    let out = reader(r).map_err(err).and_then(|r| read_limited(r, name, max, None));
    // A part missing anywhere (a palette walk that gave up, say) is read before trusting the rest.
    if let Some((a, b)) = watch.missing() {
        return Ok(Partial::Need(a, b));
    }
    out.map(|r| Partial::Done(Box::new(r)))
}

/// [`read`] from a file, decoding its strips or tiles on several threads.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_file(path: &std::path::Path, name: &str) -> Result<Raster> {
    read_file_with_nodata(path, name, None)
}

/// [`read_file`], with the no-data value a sidecar gives (see [`read_limited_with_nodata`]).
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn read_file_with_nodata(path: &std::path::Path, name: &str, sidecar_nodata: Option<f64>) -> Result<Raster> {
    let path = path.to_path_buf();
    let open = move || -> std::io::Result<Reader<'static>> { reader(std::io::BufReader::new(std::fs::File::open(&path)?)) };
    read_limited_with_nodata(open().map_err(err)?, name, max_samples(), Some(&open), sidecar_nodata)
}

/// [`read`], holding at most `max` samples (the tests' small limit exercises the overviews), with
/// `open` making more readers of the same file for decoding it on several threads.
pub(super) fn read_limited(r: Reader<'_>, name: &str, max: usize, open: Option<Opener<'_>>) -> Result<Raster> {
    read_limited_with_nodata(r, name, max, open, None)
}

/// [`read_limited`], with the no-data value a sidecar gives (`.aux.xml`, applied after reading by
/// [`Raster::apply_pam`]): when the file has none of its own, its pixels aren't counted as rounded.
pub(super) fn read_limited_with_nodata(
    r: Reader<'_>,
    name: &str,
    max: usize,
    open: Option<Opener<'_>>,
    sidecar_nodata: Option<f64>,
) -> Result<Raster> {
    // The full image says whether it is read as a palette image, whichever image is read, as a
    // COG window has it: a grey full image's stray colour table colours nothing, nor do palette
    // overviews of it.
    let palette = r.first_is_palette();
    let limits = {
        let mut l = Limits::default();
        l.decoding_buffer_size = max_samples().saturating_mul(4);
        l.intermediate_buffer_size = 256 << 20;
        l.ifd_value_size = 64 << 20;
        l
    };
    let mut dec = Decoder::new(r).map_err(err)?.with_limits(limits.clone());
    let (full_w, full_h) = dec.dimensions().map_err(err)?;
    let geo = geo_tags(&mut dec)?;
    // The no-data value whose pixels aren't counted as rounded.
    let uncounted = Uncounted::new(geo.nodata_exact, sidecar_nodata);
    // The bands' descriptions GDAL keeps in the file (an `.aux.xml` sidecar's are applied after).
    // A tag that doesn't decode is left out, and said (`Raster::lost`).
    let described = match dec.find_tag(Tag::Unknown(42112)) {
        Ok(None) => None,
        Ok(Some(v)) => match v.into_string() {
            Ok(t) => super::pam::parse_gdal_metadata(&t),
            Err(e) => Some(super::pam::Pam::losing(format!("it isn't text ({e}): band and class names are left out"))),
        },
        Err(e) => Some(super::pam::Pam::losing(format!("it doesn't decode ({e}): band and class names are left out"))),
    };
    let spp = samples(&mut dec);
    let bytes = sample_bytes(&mut dec);
    // The decoded image is held at its own pixel size before conversion to f32, so bound bytes
    // as well as samples.
    let fits = |w: u32, h: u32, spp: usize, bytes: usize| {
        let n = (w as usize).saturating_mul(h as usize).saturating_mul(spp);
        n <= max && n.saturating_mul(bytes) <= max.saturating_mul(4)
    };
    let (mut w, mut h) = (full_w, full_h);
    let mut overview = None;
    // Every k-th pixel of the full image, for a large one without overviews (1: read whole).
    let mut decimate_by = 1u32;
    // The image read: 0, the full one, or an overview.
    let mut image = 0;
    if !fits(w, h, spp, bytes) {
        // The largest overview that fits, whatever order the images are in; a damaged image
        // ends the search rather than the open.
        let mut best: Option<(usize, u32, u32)> = None;
        let mut index = 0;
        while dec.more_images() {
            if dec.next_image().is_err() {
                break;
            }
            index += 1;
            let subfile = dec.find_tag_unsigned::<u32>(Tag::NewSubfileType).ok().flatten().unwrap_or(0);
            if subfile & 4 != 0 {
                continue; // a transparency mask
            }
            let Ok((ow, oh)) = dec.dimensions() else { continue };
            let (ospp, obytes) = (samples(&mut dec), sample_bytes(&mut dec));
            if fits(ow, oh, ospp, obytes) && best.is_none_or(|(_, bw, bh)| ow as u64 * oh as u64 > bw as u64 * bh as u64) {
                best = Some((index, ow, oh));
            }
        }
        // Every k-th pixel of the full image, the fewest left out that fit.
        let mut k = 2u32;
        while !fits(full_w.div_ceil(k), full_h.div_ceil(k), spp, 4) {
            k = k.saturating_mul(2);
            if k > 1 << 20 {
                break;
            }
        }
        let (dw, dh) = (full_w.div_ceil(k), full_h.div_ceil(k));
        let decimated = (k <= 1 << 20).then_some(u64::from(dw) * u64::from(dh));
        match best {
            // An overview at least as detailed as the full image read sparsely: only it is read.
            Some((index, ow, oh)) if decimated.is_none_or(|d| u64::from(ow) * u64::from(oh) >= d) => {
                dec.seek_to_image(index).map_err(err)?;
                image = index;
                (w, h) = (ow, oh);
            }
            // No overview, or only coarse ones (a 5000-pixel orthophoto with one of 157 pixels,
            // issue #162): one is made as the full image is read.
            _ => {
                if decimated.is_none() {
                    return Err(err(format!("{full_w} × {full_h} pixels is too large to open")));
                }
                dec.seek_to_image(0).map_err(err)?;
                decimate_by = k;
                (w, h) = (dw, dh);
            }
        }
        overview = Some((full_w, full_h));
    }
    // From here on, tags (colour type, planar layout) are those of the chosen image.
    let ct = dec.colortype().map_err(err)?;
    // JPEG-compressed YCbCr, the only YCbCr read (the colour match below rejects the rest).
    let ycbcr = matches!(ct, ColorType::YCbCr(_)) && dec.find_tag_unsigned::<u16>(Tag::Compression).ok().flatten() == Some(7);
    // A palette image (shown to the decoder as grey) is coloured by its table. One whose table
    // doesn't decode, or is too short, is drawn grey, and says so (`Raster::lost`).
    let (colour_map, map_why) = if palette {
        match dec.find_tag(Tag::ColorMap) {
            Ok(Some(v)) => (Some(v), None),
            Ok(None) => (None, Some("it is a palette image with no colour table: drawn grey".to_string())),
            Err(e) => (None, Some(format!("its colour table doesn't decode ({e}): it is left out"))),
        }
    } else {
        (None, None)
    };
    // Said for a grey image, which a table would colour (an RGB overview of a palette file isn't).
    let mut table_lost = map_why.filter(|_| matches!(ct, ColorType::Gray(_)));
    let ct = match ct {
        ColorType::Gray(bits) if colour_map.is_some() => ColorType::Palette(bits),
        other => other,
    };
    let (nbands, color) = match ct {
        ColorType::Gray(_) => (1, ColorHint::Gray),
        ColorType::GrayA(_) => (2, ColorHint::GrayAlpha),
        ColorType::RGB(_) => (3, ColorHint::Rgb { alpha: false }),
        // The decoder leaves JPEG-compressed YCbCr as it is (it turns off the JPEG library's colour
        // conversion), so it is turned into RGB below (issue #692); other YCbCr data isn't read.
        ColorType::YCbCr(_) if ycbcr => (3, ColorHint::Rgb { alpha: false }),
        ColorType::RGBA(_) => (4, ColorHint::Rgb { alpha: true }),
        ColorType::Palette(bits) => {
            let table = match colour_map.map(|v| v.into_u16_vec()) {
                Some(Ok(map)) => {
                    let table = super::cog::colour_table(&map, u16::from(bits));
                    if table.is_none() {
                        table_lost = Some(format!(
                            "its colour table has {} entries, short of the {} a palette of {bits}-bit pixels needs: it is left out",
                            map.len(),
                            3usize << bits.min(16)
                        ));
                    }
                    table
                }
                Some(Err(e)) => {
                    table_lost = Some(format!("its colour table doesn't decode ({e}): it is left out"));
                    None
                }
                None => None,
            };
            (1, table.map_or(ColorHint::Gray, ColorHint::Palette))
        }
        ColorType::Multiband { num_samples, .. } => (num_samples.max(1) as usize, multiband_color(&mut dec, num_samples)),
        other => return Err(err(format!("{other:?} images aren't supported"))),
    };
    let planar = dec.find_tag_unsigned::<u16>(Tag::PlanarConfiguration).ok().flatten() == Some(2);
    let n = (w as usize) * (h as usize);
    // The decoder leaves out samples past the colour ones (RGB with an undefined fourth band, as
    // in RGBI orthophotos) a tile row at a time, and writes a padded edge tile's whole width into
    // the image: past its edge, over the start of the next row (issue #162). Such tiled images
    // are read a tile at a time, as their chunks come back only as wide as the image.
    let tile_rows = dec.find_tag_unsigned::<u32>(Tag::TileLength).ok().flatten();
    let tiled = dec.find_tag_unsigned::<u32>(Tag::TileWidth).ok().flatten().is_some();
    let extra_samples = !planar && samples(&mut dec) > nbands && tiled;
    // The decoder asserts (panics) on the padded bottom tiles of the later bands of a tiled image
    // whose bands are stored apart, read whole (issue #544): such an image is read a tile at a
    // time, which can't be done below 8 bits a sample.
    let planar_tiles = planar && nbands > 1 && tiled && bits(&mut dec) >= 8;
    // `padded`: padding rows at the bottom. Right-edge padding is fine: the decoder places it by
    // the tile's column, which is right in every band.
    let padded = tile_rows.is_none_or(|t| t == 0 || h % t != 0);
    if planar && nbands > 1 && tiled && !planar_tiles && decimate_by == 1 && padded {
        return Err(err("tiled bands stored apart at fewer than 8 bits a sample aren't supported"));
    }
    // A large image, from a file, is read a chunk at a time on several threads (not one of
    // fewer than 8 bits a sample, whose chunks pack several pixels to a byte).
    let chunked = open.is_some() && n >= 1 << 20 && bits(&mut dec) >= 8;
    let whole = if decimate_by > 1 || extra_samples || planar_tiles || chunked {
        None
    } else {
        let mut buf = DecodingResult::U8(Vec::new());
        match dec.read_image_to_buffer(&mut buf) {
            Ok(_) => Some(buf),
            // An LZW image the decoder fails on is read a chunk at a time, the chunks it fails on
            // decoded by [`read_chunk`] (issue #350).
            Err(_) if lzw(&mut dec) && bits(&mut dec) >= 8 => None,
            Err(e) => return Err(err(e)),
        }
    };
    let (mut bands, data_type, rounded) = if let Some(buf) = whole {
        // Values f32 doesn't hold, counted by band (in their own pass: most types can't have any).
        let mut rounded = vec![0u64; nbands];
        if can_round(&buf) {
            for j in 0..n.saturating_mul(nbands) {
                if !held_exactly(&buf, j, uncounted) {
                    let band = if planar { j / n.max(1) } else { j % nbands };
                    if let Some(c) = rounded.get_mut(band) {
                        *c += 1;
                    }
                }
            }
        }
        let (values, dt) = to_f32(buf, (w as usize, h as usize))?;
        if values.len() < n.saturating_mul(nbands) {
            return Err(err("couldn't read every band"));
        }
        let bands: Vec<Vec<f32>> = if nbands == 1 {
            let mut v = values;
            v.truncate(n);
            vec![v]
        } else if planar {
            let mut bands = Vec::new();
            for plane in values.chunks(n).take(nbands) {
                let mut band = super::room(n, w as usize, h as usize)?;
                band.extend_from_slice(plane);
                bands.push(band);
            }
            bands
        } else {
            // One pass over the interleaved pixels.
            let mut bands = (0..nbands).map(|_| super::room(n, w as usize, h as usize)).collect::<Result<Vec<Vec<f32>>>>()?;
            for px in values.chunks(nbands).take(n) {
                for (band, v) in bands.iter_mut().zip(px) {
                    band.push(*v);
                }
            }
            drop(values);
            bands
        };
        (bands, dt, rounded)
    } else {
        let (iw, ih) = if decimate_by > 1 { (full_w, full_h) } else { (w, h) };
        let open = open.map(|o| (o, image, limits));
        // Already one vector per band.
        decimate(&mut dec, iw, ih, nbands, planar, decimate_by, uncounted, open)?
    };
    let rounded = if rounded.iter().any(|n| *n > 0) { rounded } else { Vec::new() };

    if ycbcr {
        ycbcr_to_rgb(&mut bands);
    }
    // A scan keeps a stand-in transform, sized to the pixels read.
    if !geo.referenced {
        let mut r = Raster::new(name, w, h, bands, super::unreferenced(w, h), Crs::LonLat, geo.nodata, color, data_type)?;
        r.georeferenced = false;
        r.overview = overview;
        r.rounded = rounded;
        r.lost.extend(table_lost);
        if let Some(p) = &described {
            r.apply_pam(p, super::pam::GDAL_METADATA);
        }
        return Ok(r);
    }
    let mut transform = geo.transform;
    // How many full-image pixels one pixel read spans, across and down.
    let (sx, sy) = if decimate_by > 1 {
        // Every k-th pixel: pixels k times the size, from the same corner.
        (f64::from(decimate_by), f64::from(decimate_by))
    } else if overview.is_some() {
        (full_w as f64 / w.max(1) as f64, full_h as f64 / h.max(1) as f64)
    } else {
        (1.0, 1.0)
    };
    transform.pixel_width *= sx;
    transform.pixel_height *= sy;
    let (bands, w, h) = match geo.affine {
        Some([x, xc, xr, y, yc, yr]) => {
            let (bands, ow, oh, north_up) = super::north_up(bands, w, h, [x, xc * sx, xr * sy, y, yc * sx, yr * sy], max)?;
            transform = north_up;
            (bands, ow, oh)
        }
        None => (bands, w, h),
    };
    let crs = match geo.crs {
        Some(c) => c,
        None => {
            let right = transform.origin_x + transform.pixel_width * w as f64;
            let bottom = transform.origin_y + transform.pixel_height * h as f64;
            let degrees = [transform.origin_x, right].iter().all(|x| (-180.0..=180.0).contains(x))
                && [transform.origin_y, bottom].iter().all(|y| (-90.0..=90.0).contains(y));
            if let Some(code) = geo.unknown_epsg {
                return Err(err(super::crs::unusable_epsg(code)));
            }
            if let Some(e) = &geo.custom_error
                && !degrees
            {
                return Err(DataError::Format(e.clone()));
            }
            if geo.geographic_model || degrees {
                Crs::LonLat
            } else {
                return Err(err("the file's coordinate system isn't recognised"));
            }
        }
    };
    let mut r = Raster::new(name, w, h, bands, transform, crs, geo.nodata, color, data_type)?;
    r.overview = overview;
    r.rounded = rounded;
    r.lost.extend(table_lost);
    if let Some(p) = &described {
        r.apply_pam(p, super::pam::GDAL_METADATA);
    }
    Ok(r)
}

/// The GeoKey directory a raster's coordinate system is written as, and the user-defined system
/// whose double and ASCII parameters go with it.
pub(super) struct GeoKeys {
    pub keys: Vec<u16>,
    pub custom: Option<std::sync::Arc<CustomCrs>>,
}

pub(super) fn geo_keys(r: &Raster) -> Result<GeoKeys> {
    let geographic = r.crs.is_geographic();
    // A file's own projection is written back as the keys it came in.
    let custom = r.crs.custom_def().map(|d| {
        // A system read from WKT (or a saved project) has no keys: they're made from its PROJ
        // definition when they can be.
        if d.geokeys.is_empty()
            && let Some((geokeys, doubles, ascii)) = keys_from_proj4(&d.proj4, &d.label)
        {
            std::sync::Arc::new(CustomCrs { geokeys, doubles, ascii, ..(*d).clone() })
        } else {
            d
        }
    });
    let keys: Vec<u16> = match &custom {
        Some(d) if d.geokeys.is_empty() => {
            return Err(err(format!("{}: a coordinate system without GeoTIFF keys can't be written", d.label)));
        }
        Some(d) => d.geokeys.clone(),
        None => vec![
            1,
            1,
            0,
            3,
            1024,
            0,
            1,
            if geographic { 2 } else { 1 },
            1025,
            0,
            1,
            1,
            if geographic { 2048 } else { 3072 },
            0,
            1,
            r.crs.epsg(),
        ],
    };
    Ok(GeoKeys { keys, custom })
}

/// Whether band `b` is written as bytes: whole numbers within 0–255 (or no valid pixels at all),
/// and a no-data value that is one too.
pub(super) fn fits_bytes(r: &Raster, b: usize) -> bool {
    let Some(band) = r.bands.get(b) else { return false };
    let s = &band.stats;
    let whole_bytes = s.min >= 0.0 && s.max <= 255.0 && (s.categories.is_some() || r.data_type == "UInt8");
    let nodata_ok = r.nodata.is_none_or(|d| (0.0..=255.0).contains(&d) && d.fract() == 0.0);
    nodata_ok && (whole_bytes || s.count == 0)
}

/// The bands [`write`] writes of `r` given `band`: an RGB raster's colour bands (with a fourth as
/// alpha when they are bytes and it is too, or when the raster says it is alpha), otherwise `band`
/// alone.
pub fn written_bands(r: &Raster, band: usize) -> Vec<usize> {
    if !(matches!(r.color, ColorHint::Rgb { .. }) && r.bands.len() >= 3) {
        return vec![band];
    }
    let alpha = if (0..3).all(|b| fits_bytes(r, b)) {
        r.bands.len() >= 4 && fits_bytes(r, 3)
    } else {
        r.bands.len() >= 4 && matches!(r.color, ColorHint::Rgb { alpha: true })
    };
    (0..if alpha { 4 } else { 3 }).collect()
}

/// Writes a raster as a Deflate-compressed GeoTIFF: a colour raster's colour bands (RGB(A) bytes
/// when they are 8-bit, else 32-bit floats, alpha kept), so it reads back in colour; otherwise
/// `band` (0-based) as 32-bit floats. `band` is for rasters that aren't colour ones: a colour
/// raster always writes its colour bands, as its 8-bit form did before. A cancelled run stops
/// it (with [`DataError::Cancelled`]) within a strip.
pub fn write(r: &Raster, band: usize) -> Result<Vec<u8>> {
    match encode(r, band) {
        Ok(bytes) => Ok(bytes),
        // The encoder's error for a cancel only says the write stopped.
        Err(e) => crate::progress::check().and(Err(e)),
    }
}

/// The file's bytes, written by the encoder a strip's compressed output at a time: each write
/// fails once the run was cancelled. It counts no progress: the encoder doesn't say where a
/// strip ends.
struct Probed<W>(W);

impl<W: std::io::Write> std::io::Write for Probed<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        crate::progress::check().map_err(std::io::Error::other)?;
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl<W: Seek> Seek for Probed<W> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

/// [`write`], a cancel failing as any write error does.
fn encode(r: &Raster, band: usize) -> Result<Vec<u8>> {
    use tiff::encoder::{Compression, DeflateLevel, TiffEncoder, colortype};
    // A scan's place is only where it was laid on the view: not coordinates to write into a file.
    r.require_coordinates().map_err(err)?;
    let mut out = Probed(std::io::Cursor::new(Vec::new()));
    let mut enc = TiffEncoder::new(&mut out).map_err(err)?.with_compression(Compression::Deflate(DeflateLevel::Balanced));
    let gt = r.transform;
    if gt.pixel_height > 0.0 || gt.pixel_width < 0.0 {
        return Err(err("only north-up rasters can be exported for now"));
    }
    let scale = [gt.pixel_width.abs(), gt.pixel_height.abs(), 0.0];
    let tie = [0.0, 0.0, 0.0, gt.origin_x, gt.origin_y, 0.0];
    let GeoKeys { keys, custom } = geo_keys(r)?;
    let (w, h) = (r.width, r.height);
    let bytes = |b: usize| -> Option<Vec<u8>> {
        let band = r.bands.get(b)?;
        // No-data pixels keep the no-data value, so the written GDAL_NODATA tag still matches.
        let nodata = r.nodata.map(|d| d as u8);
        fits_bytes(r, b)
            .then(|| band.data.iter().map(|v| if r.is_valid(*v) { v.clamp(0.0, 255.0) as u8 } else { nodata.unwrap_or(0) }).collect())
    };
    let rgb = matches!(r.color, ColorHint::Rgb { .. }) && r.bands.len() >= 3;
    // Each branch below names the bands it writes as samples (`$written`): their names, and the
    // first one's class names (the red band's, for a colour file), go to GDAL's metadata tag,
    // read back on open.
    macro_rules! tags {
        ($img:expr, $written:expr) => {{
            let classes = super::gdal_metadata_of(r, $written);
            let e = $img.encoder();
            e.write_tag(Tag::ModelPixelScaleTag, &scale[..]).map_err(err)?;
            e.write_tag(Tag::ModelTiepointTag, &tie[..]).map_err(err)?;
            e.write_tag(Tag::GeoKeyDirectoryTag, &keys[..]).map_err(err)?;
            if let Some(d) = &custom {
                if !d.doubles.is_empty() {
                    e.write_tag(Tag::Unknown(34736), &d.doubles[..]).map_err(err)?;
                }
                if !d.ascii.is_empty() {
                    e.write_tag(Tag::Unknown(34737), d.ascii.as_str()).map_err(err)?;
                }
            }
            if let Some(xml) = &classes {
                e.write_tag(Tag::Unknown(42112), xml.as_str()).map_err(err)?;
            }
            if let Some(nd) = r.nodata {
                e.write_tag(Tag::GdalNodata, nd.to_string().as_str()).map_err(err)?;
            }
        }};
    }
    if rgb && let (Some(red), Some(green), Some(blue)) = (bytes(0), bytes(1), bytes(2)) {
        let alpha = if written_bands(r, band).contains(&3) { bytes(3) } else { None };
        let n = (w as usize) * (h as usize);
        if let Some(a) = alpha {
            let mut data = Vec::with_capacity(n * 4);
            for i in 0..n {
                data.extend([red.get(i), green.get(i), blue.get(i), a.get(i)].map(|v| v.copied().unwrap_or(0)));
            }
            let mut img = enc.new_image::<colortype::RGBA8>(w, h).map_err(err)?;
            tags!(img, &[0, 1, 2, 3]);
            img.write_data(&data).map_err(err)?;
        } else {
            let mut data = Vec::with_capacity(n * 3);
            for i in 0..n {
                data.extend([red.get(i), green.get(i), blue.get(i)].map(|v| v.copied().unwrap_or(0)));
            }
            let mut img = enc.new_image::<colortype::RGB8>(w, h).map_err(err)?;
            tags!(img, &[0, 1, 2]);
            img.write_data(&data).map_err(err)?;
        }
    } else if rgb && let (Some(red), Some(green), Some(blue)) = (r.bands.first(), r.bands.get(1), r.bands.get(2)) {
        // Colour bands that aren't bytes (three embedding dimensions): kept as 32-bit floats, with
        // their alpha band when they have one.
        let n = (w as usize) * (h as usize);
        let at = |b: &crate::raster::Band, i: usize| b.data.get(i).copied().unwrap_or(f32::NAN);
        match r.bands.get(3).filter(|_| written_bands(r, band).contains(&3)) {
            Some(alpha) => {
                let mut data = Vec::with_capacity(n * 4);
                for i in 0..n {
                    data.extend([at(red, i), at(green, i), at(blue, i), at(alpha, i)]);
                }
                let mut img = enc.new_image::<colortype::RGBA32Float>(w, h).map_err(err)?;
                tags!(img, &[0, 1, 2, 3]);
                img.write_data(&data).map_err(err)?;
            }
            None => {
                let mut data = Vec::with_capacity(n * 3);
                for i in 0..n {
                    data.extend([at(red, i), at(green, i), at(blue, i)]);
                }
                let mut img = enc.new_image::<colortype::RGB32Float>(w, h).map_err(err)?;
                tags!(img, &[0, 1, 2]);
                img.write_data(&data).map_err(err)?;
            }
        }
    } else {
        let b = r.bands.get(band).ok_or_else(|| err(format!("the raster has no band {}", band + 1)))?;
        let mut img = enc.new_image::<colortype::Gray32Float>(w, h).map_err(err)?;
        tags!(img, &[band]);
        img.write_data(&b.data).map_err(err)?;
    }
    Ok(out.0.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::tests::small;

    #[test]
    fn float_colour_round_trip() {
        let r = small();
        let n = (r.width * r.height) as usize;
        let bands = (0..3).map(|b| (0..n).map(|i| i as f32 * 0.5 - b as f32).collect()).collect();
        let c = Raster::new("c", r.width, r.height, bands, r.transform, r.crs, None, ColorHint::Rgb { alpha: false }, "Float32").unwrap();
        let back = read(std::io::Cursor::new(write(&c, 0).unwrap()), "back").unwrap();
        assert_eq!(back.bands.len(), 3);
        assert_eq!(back.value(2, 1, 1), c.value(2, 1, 1));
        assert!(matches!(back.color, ColorHint::Rgb { .. }));
        // With an alpha band: four bands back.
        let bands = (0..4).map(|b| (0..n).map(|i| i as f32 * 0.5 - b as f32).collect()).collect();
        let a = Raster::new("a", r.width, r.height, bands, r.transform, r.crs, None, ColorHint::Rgb { alpha: true }, "Float32").unwrap();
        let back = read(std::io::Cursor::new(write(&a, 0).unwrap()), "back").unwrap();
        assert_eq!(back.bands.len(), 4);
        assert_eq!(back.value(3, 1, 1), a.value(3, 1, 1));
    }

    #[test]
    fn float_round_trip() {
        let r = small();
        let bytes = write(&r, 0).unwrap();
        let back = read(std::io::Cursor::new(&bytes), "back").unwrap();
        assert_eq!((back.width, back.height), (4, 3));
        assert_eq!(back.crs, Crs::LonLat);
        assert_eq!(back.nodata, Some(-9999.0));
        assert_eq!(back.transform, r.transform);
        assert_eq!(back.value(0, 2, 1), Some(7.0));
        assert_eq!(back.value(0, 1, 1), None);
        assert_eq!(back.data_type, "Float32");
    }

    #[test]
    fn rotated_matrix_is_turned_north_up() {
        // Issue #251: GDAL's geotransform (400000, 30, 5, 5000000, 3, -30).
        let m = [30.0, 5.0, 0.0, 400_000.0, 3.0, -30.0, 0.0, 5_000_000.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let r = read(std::io::Cursor::new(tests_support::matrix_tiff(m, false)), "rot").unwrap();
        assert_eq!(r.crs, Crs::from_epsg(32633).unwrap());
        // gdalinfo's corners: x from 400000 to 400650, y from 4999700 to 5000060; pixels of the
        // same area (30 × 30 + 5 × 3).
        let gt = r.transform;
        let cell = (30.0f64 * 30.0 + 5.0 * 3.0).sqrt();
        assert!((gt.pixel_width - cell).abs() < 1e-9 && (gt.pixel_height + cell).abs() < 1e-9);
        assert_eq!((gt.origin_x, gt.origin_y), (400_000.0, 5_000_060.0));
        let (right, bottom) = (gt.origin_x + gt.pixel_width * f64::from(r.width), gt.origin_y + gt.pixel_height * f64::from(r.height));
        assert!((400_650.0..400_650.0 + cell).contains(&right), "{right}");
        assert!((4_999_700.0 - cell..=4_999_700.0).contains(&bottom), "{bottom}");
        // The pixels sit where the matrix puts them; the box's corners outside the image are empty.
        let at = |x: f64, y: f64| {
            let (c, row) = gt.to_pixel(rustgis_geom::Coord::new(x, y));
            r.value(0, c.floor() as i64, row.floor() as i64)
        };
        let world = |c: f64, row: f64| (400_000.0 + 30.0 * c + 5.0 * row, 5_000_000.0 + 3.0 * c - 30.0 * row);
        let (x, y) = world(0.5, 0.5);
        assert_eq!(at(x, y), Some(7.0));
        let (x, y) = world(19.5, 9.5);
        assert_eq!(at(x, y), Some(9.0));
        let (x, y) = world(10.0, 2.5);
        assert_eq!(at(x, y), Some(100.0));
        let (x, y) = world(10.0, 7.5);
        assert_eq!(at(x, y), Some(200.0));
        assert_eq!(r.value(0, 0, 0), None);
        assert_eq!(r.value(0, i64::from(r.width) - 1, i64::from(r.height) - 1), None);

        // A point's value at a pixel's centre: the image starts half a pixel up and left.
        let p = read(std::io::Cursor::new(tests_support::matrix_tiff(m, true)), "rot").unwrap();
        assert_eq!((p.transform.origin_x, p.transform.origin_y), (400_000.0 - 17.5, 5_000_060.0 - 1.5 + 15.0));

        // Read every 4th pixel (a small limit): the matrix's steps scale, so the box stays put.
        let file = tests_support::matrix_tiff(m, false);
        let d = read_limited(reader(std::io::Cursor::new(&file)).unwrap(), "rot", 50, None).unwrap();
        assert_eq!(d.overview, Some((20, 10)));
        assert!(d.width as usize * d.height as usize <= 50);
        let dg = d.transform;
        assert_eq!((dg.origin_x, dg.origin_y), (400_000.0, 5_000_060.0));
        let (right, bottom) = (dg.origin_x + dg.pixel_width * f64::from(d.width), dg.origin_y + dg.pixel_height * f64::from(d.height));
        assert!((400_650.0..400_650.0 + dg.pixel_width).contains(&right), "{right}");
        assert!((4_999_700.0 + dg.pixel_height..=4_999_700.0).contains(&bottom), "{bottom}");

        // A degenerate matrix (no area) is refused, not divided by.
        let flat = [30.0, 60.0, 0.0, 0.0, 1.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        assert!(read(std::io::Cursor::new(tests_support::matrix_tiff(flat, false)), "flat").is_err());
    }

    #[test]
    fn rgb_round_trip_in_utm() {
        let gt = GeoTransform { origin_x: 500_000.0, origin_y: 4_000_000.0, pixel_width: 30.0, pixel_height: -30.0 };
        let bands = vec![vec![10.0, 20.0], vec![30.0, 40.0], vec![50.0, 60.0]];
        let r =
            Raster::new("rgb", 2, 1, bands, gt, Crs::from_epsg(32617).unwrap(), None, ColorHint::Rgb { alpha: false }, "UInt8").unwrap();
        let back = read(std::io::Cursor::new(write(&r, 0).unwrap()), "back").unwrap();
        assert_eq!(back.band_count(), 3);
        assert_eq!(back.crs.epsg(), 32617);
        assert_eq!(back.color, ColorHint::Rgb { alpha: false });
        assert_eq!(back.value(2, 1, 0), Some(60.0));
        assert_eq!(back.data_type, "UInt8");
    }

    /// An Equi7 tile (a Sentinel-1 product on TU Wien's grid, say) names its system by its EPSG
    /// code, 27707 for South America: read as that system, not refused.
    #[test]
    fn an_equi7_tile_is_read() {
        let gt = GeoTransform { origin_x: 8_400_000.0, origin_y: 5_400_000.0, pixel_width: 20.0, pixel_height: -20.0 };
        let r =
            Raster::new("e7", 2, 2, vec![vec![1.0, 2.0, 3.0, 4.0]], gt, Crs::from_epsg(27707).unwrap(), None, ColorHint::Gray, "Float32")
                .unwrap();
        let back = read(std::io::Cursor::new(write(&r, 0).unwrap()), "back").unwrap();
        assert_eq!(back.crs.epsg(), 27707);
        // Brasília is in it: about 8,609 km east and 5,356 km north of the grid's corner.
        let at = back.crs.transformer().unwrap().to_lonlat(rustgis_geom::Coord::new(8608845.8731, 5356292.7056)).unwrap();
        assert!((at.x + 47.88).abs() < 1e-6 && (at.y + 15.79).abs() < 1e-6, "{at:?}");
    }

    #[test]
    fn rgb_export_keeps_no_data() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 2.0, pixel_width: 1.0, pixel_height: -1.0 };
        let mk = |nodata: f64| {
            let bands = vec![vec![10.0, nodata as f32], vec![20.0, nodata as f32], vec![30.0, nodata as f32]];
            Raster::new("rgb", 2, 1, bands, gt, Crs::LonLat, Some(nodata), ColorHint::Rgb { alpha: false }, "UInt8").unwrap()
        };
        // A byte no-data value survives on the byte path.
        let back = read(std::io::Cursor::new(write(&mk(255.0), 0).unwrap()), "b").unwrap();
        assert_eq!((back.band_count(), back.value(0, 1, 0), back.value(0, 0, 0)), (3, None, Some(10.0)));
        // One that doesn't fit a byte keeps the file honest by writing the three bands as floats.
        let back = read(std::io::Cursor::new(write(&mk(-9999.0), 0).unwrap()), "f").unwrap();
        assert_eq!((back.band_count(), back.value(0, 1, 0), back.nodata), (3, None, Some(-9999.0)));
    }

    #[test]
    fn a_wkt_projection_is_written_as_keys() {
        // HRRR's grid, as its Zarr store's WKT says it: Lambert conformal conic on a sphere.
        let wkt = r#"PROJCS["unnamed",GEOGCS["GRIB",DATUM["unnamed",SPHEROID["Sphere",6371229,0]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Lambert_Conformal_Conic_2SP"],PARAMETER["latitude_of_origin",38.5],PARAMETER["central_meridian",-97.5],PARAMETER["standard_parallel_1",38.5],PARAMETER["standard_parallel_2",38.5],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["Metre",1]]"#;
        let crs = Crs::parse(wkt).unwrap();
        let gt = GeoTransform { origin_x: -2_699_020.0, origin_y: 1_588_193.0, pixel_width: 3000.0, pixel_height: -3000.0 };
        let r = Raster::new("hrrr", 2, 2, vec![vec![1.0, 2.0, 3.0, 4.0]], gt, crs, None, ColorHint::Gray, "Float32").unwrap();
        let back = read(std::io::Cursor::new(write(&r, 0).unwrap()), "hrrr").unwrap();
        let p = back.crs.custom_def().unwrap().proj4.clone();
        assert!(p.contains("+proj=lcc") && p.contains("+lon_0=-97.5") && p.contains("+a=6371229"), "{p}");
        // The same place either way.
        let c = rustgis_geom::Coord { x: 0.0, y: 0.0 };
        let (a, b) = (r.crs.transformer().unwrap().to_lonlat(c).unwrap(), back.crs.transformer().unwrap().to_lonlat(c).unwrap());
        assert!((a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9 && (a.x + 97.5).abs() < 1e-6, "{a:?} {b:?}");
        // One the keys can't say is refused, as before.
        assert!(keys_from_proj4("+proj=robin +lon_0=0 +datum=WGS84", "x").is_none());
        assert!(keys_from_proj4("junk", "x").is_none());
        assert!(keys_from_proj4("+proj=tmerc +lon_0=0 +units=us-ft +datum=WGS84", "x").is_none(), "only metres");
        for unsaid in [
            "+proj=stere +lat_0=45 +lat_ts=30 +datum=WGS84",
            "+proj=lcc +lat_1=30 +lat_0=40 +lon_0=-96 +datum=WGS84",
            "+proj=eqc +lat_0=30 +lon_0=0 +datum=WGS84",
            "+proj=laea +lat_0=52 +lon_0=10 +k_0=0.9 +datum=WGS84",
            "+proj=lcc +lat_1=30 +lat_2=40 +k_0=0.9996 +datum=WGS84",
            "+proj=merc +lat_ts=33 +datum=WGS84",
            "+proj=tmerc +ellps=bessel +towgs84=598.1,73.7,418.2",
            "+proj=lcc +lat_1=40 +pm=paris +a=6378249.2 +b=6356515",
        ] {
            assert!(keys_from_proj4(unsaid, "x").is_none(), "{unsaid}");
        }
        // A one-parallel conic with a scale factor (as WKT's 1SP gives it, both parallels the same)
        // keeps it.
        let (k, d, a) =
            keys_from_proj4("+proj=lcc +lat_1=46.8 +lat_2=46.8 +lat_0=46.8 +lon_0=2.3 +k_0=0.99987742 +datum=WGS84", "x").unwrap();
        assert!(custom_crs(&k, &d, &a).unwrap().custom_def().unwrap().proj4.contains("+k_0=0.99987742"));
        // A polar stereographic round trip: the pole's longitude comes back.
        let (k, d, a) =
            keys_from_proj4("+proj=stere +lat_0=90 +lat_ts=70 +lon_0=-45 +k=1 +x_0=0 +y_0=0 +datum=WGS84 +units=m", "NSIDC").unwrap();
        let back = custom_crs(&k, &d, &a).unwrap().custom_def().unwrap().proj4.clone();
        assert!(back.contains("+lat_0=90") && back.contains("+lat_ts=70") && back.contains("+lon_0=-45"), "{back}");
    }

    #[test]
    fn south_up_export_is_refused() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 0.0, pixel_width: 1.0, pixel_height: 1.0 };
        let r = Raster::new("s", 1, 1, vec![vec![1.0]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        assert!(write(&r, 0).is_err());
    }

    #[test]
    fn plain_tiff_opens_as_a_scan() {
        use tiff::encoder::{TiffEncoder, colortype};
        let mut out = std::io::Cursor::new(Vec::new());
        TiffEncoder::new(&mut out).unwrap().write_image::<colortype::Gray8>(2, 2, &[1, 2, 3, 4]).unwrap();
        let r = read(std::io::Cursor::new(out.into_inner()), "x").unwrap();
        assert_eq!((r.width, r.height, r.georeferenced), (2, 2, false));
    }

    #[test]
    fn junk_never_panics() {
        let r = small();
        let good = write(&r, 0).unwrap();
        for cut in [0, 8, 16, 100, good.len() / 2, good.len() - 1] {
            let _ = read(std::io::Cursor::new(&good[..cut]), "x");
        }
        let mut flipped = good.clone();
        for i in (8..flipped.len()).step_by(7) {
            flipped[i] ^= 0x5a;
        }
        let _ = read(std::io::Cursor::new(&flipped), "x");
    }

    /// A grey 8-bit GeoTIFF in longitude/latitude, from the encoder, with `extra` tags.
    fn grey8(w: u32, h: u32, data: &[u8], colormap: Option<&[u16]>) -> Vec<u8> {
        use tiff::encoder::{TiffEncoder, colortype};
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut enc = TiffEncoder::new(&mut out).unwrap();
            let mut img = enc.new_image::<colortype::Gray8>(w, h).unwrap();
            let e = img.encoder();
            e.write_tag(Tag::ModelPixelScaleTag, &[0.5f64, 0.5, 0.0][..]).unwrap();
            e.write_tag(Tag::ModelTiepointTag, &[0.0f64, 0.0, 0.0, 10.0, 50.0, 0.0][..]).unwrap();
            e.write_tag(Tag::GeoKeyDirectoryTag, &[1u16, 1, 0, 2, 1024, 0, 1, 2, 2048, 0, 1, 4326][..]).unwrap();
            if let Some(m) = colormap {
                e.write_tag(Tag::ColorMap, m).unwrap();
            }
            img.rows_per_strip(4).unwrap();
            img.write_data(data).unwrap();
        }
        out.into_inner()
    }

    /// Sets a classic little-endian TIFF's PhotometricInterpretation (as a palette writer would).
    fn set_photometric(b: &mut [u8], value: u16) {
        let ifd = u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize;
        let n = u16::from_le_bytes(b[ifd..ifd + 2].try_into().unwrap()) as usize;
        for i in 0..n {
            let e = ifd + 2 + i * 12;
            if u16::from_le_bytes(b[e..e + 2].try_into().unwrap()) == 262 {
                b[e + 8..e + 10].copy_from_slice(&value.to_le_bytes());
            }
        }
    }

    /// Recompresses a classic little-endian TIFF's strips with Zstandard (as GDAL's
    /// `COMPRESS=ZSTD` does), appending them and repointing the strip offsets.
    #[cfg(not(target_arch = "wasm32"))]
    fn zstd_strips(b: &mut Vec<u8>) {
        let u16_at = |b: &[u8], i: usize| u16::from_le_bytes(b[i..i + 2].try_into().unwrap());
        let u32_at = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as usize;
        let ifd = u32_at(b, 4);
        // A tag's directory entry.
        let entry = |b: &[u8], tag: u16| (0..u16_at(b, ifd) as usize).map(|i| ifd + 2 + i * 12).find(|&e| u16_at(b, e) == tag).unwrap();
        let (offsets, counts) = (entry(b, 273), entry(b, 279));
        assert_eq!((u16_at(b, offsets + 2), u16_at(b, counts + 2)), (4, 4), "LONG strip tags");
        let n = u32_at(b, offsets + 4);
        let at = |b: &[u8], e: usize| if n == 1 { e + 8 } else { u32_at(b, e + 8) };
        let (offsets, counts) = (at(b, offsets), at(b, counts));
        for i in 0..n {
            let (start, len) = (u32_at(b, offsets + i * 4), u32_at(b, counts + i * 4));
            let packed = zstd::encode_all(&b[start..start + len], 3).unwrap();
            let new_start = u32::try_from(b.len()).unwrap();
            let new_len = u32::try_from(packed.len()).unwrap();
            b.extend(packed);
            b[offsets + i * 4..offsets + i * 4 + 4].copy_from_slice(&new_start.to_le_bytes());
            b[counts + i * 4..counts + i * 4 + 4].copy_from_slice(&new_len.to_le_bytes());
        }
        let compression = entry(b, 259);
        b[compression + 8..compression + 10].copy_from_slice(&50000u16.to_le_bytes());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn zstd_compressed_files_are_read() {
        // Issue #134: Zstandard is a usual compression for COGs (EnMAP's, among others).
        let (w, h) = (8u32, 8u32);
        let data: Vec<u8> = (0..w * h).map(|i| (i * 3 % 251) as u8).collect();
        let mut bytes = grey8(w, h, &data, None);
        zstd_strips(&mut bytes);
        let r = read(std::io::Cursor::new(&bytes), "zstd").unwrap();
        assert_eq!((r.width, r.height), (8, 8));
        for (x, y) in [(0i64, 0i64), (7, 0), (3, 5), (7, 7)] {
            assert_eq!(r.value(0, x, y), Some(f32::from(data[(y * 8 + x) as usize])), "{x},{y}");
        }
    }

    /// Reads `file` as the web app does: parts of `chunk` bytes, as [`read_sparse_limited`] asks
    /// for them. Returns the raster and the parts read.
    fn read_in_parts(file: &[u8], chunk: u64, max: usize) -> (Raster, Sparse) {
        let len = file.len() as u64;
        let mut parts = Sparse::default().with_len(len).with(0, file[..chunk.min(len) as usize].to_vec());
        for _ in 0..1000 {
            match read_sparse_limited(&parts, "parts", max).unwrap() {
                Partial::Done(r) => return (*r, parts),
                Partial::Need(a, b) => {
                    assert!(a < len, "a read past the end: {a}..{b}");
                    let end = b.max(a + chunk).min(len).min(parts.next_start(a).unwrap_or(len));
                    parts = parts.with(a, file[a as usize..end as usize].to_vec());
                }
            }
        }
        panic!("too many rounds");
    }

    #[test]
    fn a_cog_read_in_parts_reads_only_its_overview() {
        // 2048 pixels a side in 256-pixel tiles, with overviews of 1024, 512 and 256; at most a
        // million samples, so the 1024 overview is read, and nothing of the full image's tiles.
        let file = super::super::cog::example(2048, 256);
        let whole = read_limited(reader(std::io::Cursor::new(&file)).unwrap(), "whole", 1 << 20, None).unwrap();
        assert_eq!((whole.width, whole.height), (1024, 1024));
        let (r, parts) = read_in_parts(&file, 64 << 10, 1 << 20);
        assert_eq!((r.width, r.height, r.overview), (whole.width, whole.height, whole.overview));
        assert_eq!(r.transform, whole.transform);
        for (x, y) in [(0i64, 0i64), (1023, 0), (517, 300), (1023, 1023)] {
            assert_eq!(r.value(0, x, y), whole.value(0, x, y), "{x},{y}");
        }
        // The full image is 4 MB of the file's 5.6 MB; well under that is read.
        assert!(parts.held() < file.len() as u64 / 3, "{} of {}", parts.held(), file.len());
        // Kept as a container and read back (with the same limit), the same raster.
        let kept = Sparse::from_container(&parts.container()).unwrap();
        let Partial::Done(back) = read_sparse_limited(&kept, "back", 1 << 20).unwrap() else { panic!("parts missing") };
        assert_eq!((back.width, back.height), (r.width, r.height));
        assert_eq!(back.value(0, 517, 300), r.value(0, 517, 300));
    }

    #[test]
    fn a_striped_file_read_in_parts_is_read_to_its_end() {
        // No overviews and in strips: every strip is needed, and reads stop at the file's end.
        let (w, h) = (301u32, 203u32);
        let data: Vec<u8> = (0..w * h).map(|i| ((i % w + i / w) % 251) as u8).collect();
        let bytes = grey8(w, h, &data, None);
        let whole = read_limited(reader(std::io::Cursor::new(&bytes)).unwrap(), "big", 10_000, None).unwrap();
        let (r, _) = read_in_parts(&bytes, 1000, 10_000);
        assert_eq!((r.width, r.height), (whole.width, whole.height));
        assert_eq!(r.value(0, 30, 7), whole.value(0, 30, 7));
    }

    #[test]
    fn a_part_container_opens_as_a_tif() {
        let file = super::super::cog::example(512, 256);
        // Read as the web app reads, with its limit, as the saved parts are read back anywhere.
        let (r, parts) = read_in_parts(&file, 4096, super::super::BROWSER_SAMPLES);
        let back = super::super::read_bytes("parts.tif", &parts.container()).unwrap();
        assert_eq!((back.width, back.height, back.value(0, 300, 200)), (r.width, r.height, r.value(0, 300, 200)));
    }

    #[test]
    fn damaged_part_containers_never_panic() {
        let file = super::super::cog::example(512, 256);
        let (_, parts) = read_in_parts(&file, 4096, max_samples());
        let good = parts.container();
        for cut in (0..good.len()).step_by(97) {
            let _ = super::super::read_bytes("x.tif", &good[..cut]);
        }
        let mut flipped = good.clone();
        for i in (16..flipped.len()).step_by(13) {
            flipped[i] ^= 0x5a;
        }
        let _ = super::super::read_bytes("x.tif", &flipped);
        // A container that says it holds more parts than it does is refused.
        let mut lying = good[..32].to_vec();
        lying[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Sparse::from_container(&lying).is_none());
        // Parts at the largest offsets, or past the file's end, are refused or read safely.
        let container = |len: u64, at: u64, part: &[u8]| {
            let mut c = good[..16].to_vec();
            for v in [len, 1, at, part.len() as u64] {
                c.extend_from_slice(&v.to_le_bytes());
            }
            c.extend_from_slice(part);
            c
        };
        assert!(Sparse::from_container(&container(10, 8, b"abcd")).is_none());
        assert!(Sparse::from_container(&container(u64::MAX, u64::MAX - 1, b"abcd")).is_none());
        let near_end = Sparse::from_container(&container(u64::MAX, u64::MAX - 8, b"abcd")).unwrap();
        let mut r = near_end.reader();
        assert!(std::io::Seek::seek(&mut r, std::io::SeekFrom::End(0)).is_ok());
        let _ = super::super::read_bytes("x.tif", &container(u64::MAX, u64::MAX - 8, b"II*\0"));
    }

    #[test]
    fn palette_images_read_their_colour_table() {
        let data: Vec<u8> = (0..64u32).map(|i| (i % 3) as u8).collect();
        // A 256-entry map: index 1 red, 2 green.
        let mut map = vec![0u16; 768];
        map[1] = 65535;
        map[256 + 2] = 65535;
        let mut bytes = grey8(8, 8, &data, Some(&map));
        set_photometric(&mut bytes, 3);
        let r = read(std::io::Cursor::new(&bytes), "pal").unwrap();
        match &r.color {
            ColorHint::Palette(t) => {
                assert_eq!(t[1], [255, 0, 0, 255]);
                assert_eq!(t[2], [0, 255, 0, 255]);
            }
            other => panic!("not a palette: {other:?}"),
        }
        assert_eq!(r.value(0, 1, 0), Some(1.0));
        assert_eq!(r.value(0, 2, 0), Some(2.0));
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        // A table too short for its bytes: drawn grey, and said.
        let mut short = grey8(8, 8, &data, Some(&map[..384]));
        set_photometric(&mut short, 3);
        let grey = read(std::io::Cursor::new(&short), "short").unwrap();
        assert!(matches!(grey.color, ColorHint::Gray), "{:?}", grey.color);
        assert_eq!(grey.lost, ["its colour table has 384 entries, short of the 768 a palette of 8-bit pixels needs: it is left out"]);
        let mut none = grey8(8, 8, &data, None);
        set_photometric(&mut none, 3);
        let none = read(std::io::Cursor::new(&none), "none").unwrap();
        assert_eq!(none.lost, ["it is a palette image with no colour table: drawn grey"]);
        assert!(matches!(none.color, ColorHint::Gray), "{:?}", none.color);
        // Every truncation of a palette file is an error, not a panic.
        for n in (0..bytes.len()).step_by(11) {
            let _ = read(std::io::Cursor::new(&bytes[..n]), "x");
        }
    }

    #[test]
    fn large_files_without_overviews_are_read_as_one() {
        // Over a 10,000-sample limit, so every 4th pixel each way is kept.
        // 301 × 203 in 4-row strips: the last strip is short, the width odd.
        let (w, h) = (301u32, 203u32);
        let data: Vec<u8> = (0..w * h).map(|i| ((i % w + i / w) % 251) as u8).collect();
        let bytes = grey8(w, h, &data, None);
        let full = read(std::io::Cursor::new(&bytes), "full").unwrap();
        let r = read_limited(reader(std::io::Cursor::new(&bytes)).unwrap(), "big", 10_000, None).unwrap();
        assert_eq!((r.width, r.height), (76, 51));
        assert_eq!(r.overview, Some((301, 203)));
        assert_eq!(r.transform.pixel_width, full.transform.pixel_width * 4.0);
        assert_eq!((r.transform.origin_x, r.transform.origin_y), (full.transform.origin_x, full.transform.origin_y));
        for (x, y) in [(0i64, 0i64), (1, 1), (75, 50), (74, 49), (30, 7)] {
            assert_eq!(r.value(0, x, y), full.value(0, x * 4, y * 4), "{x},{y}");
        }
    }

    /// Reads `bytes` as from a file, decoding its chunks on several threads.
    fn read_threaded(bytes: &[u8], max: usize) -> Result<Raster> {
        let bytes = std::sync::Arc::new(bytes.to_vec());
        let open = move || reader(std::io::Cursor::new(bytes.as_slice().to_vec()));
        read_limited(open().unwrap(), "threaded", max, Some(&open))
    }

    #[test]
    fn chunks_decoded_on_several_threads_match_one() {
        // 1031 × 1029 in 4-row strips: 258 strips, over a million pixels, so read a chunk at a
        // time; the last strip short, the width odd.
        let (w, h) = (1031u32, 1029u32);
        let data: Vec<u8> = (0..w * h).map(|i| ((i % w * 7 + i / w * 3) % 253) as u8).collect();
        let bytes = grey8(w, h, &data, None);
        let one = read(std::io::Cursor::new(&bytes), "one").unwrap();
        let many = read_threaded(&bytes, max_samples()).unwrap();
        assert_eq!((many.width, many.height), (w, h));
        assert!(many.bands.iter().zip(&one.bands).all(|(a, b)| a.data == b.data) && many.bands.len() == one.bands.len());
        assert_eq!(many.data_type, one.data_type);
        // Made smaller as it's read: every 4th pixel, as on one thread.
        let one = read_limited(reader(std::io::Cursor::new(&bytes)).unwrap(), "one", 100_000, None).unwrap();
        let many = read_threaded(&bytes, 100_000).unwrap();
        assert_eq!((many.width, many.height, many.overview), (one.width, one.height, Some((w, h))));
        assert!(many.bands.iter().zip(&one.bands).all(|(a, b)| a.data == b.data) && many.bands.len() == one.bands.len());
        assert_eq!(many.transform, one.transform);
    }

    #[test]
    fn a_damaged_chunk_fails_the_threaded_read() {
        let (w, h) = (1031u32, 1029u32);
        let data = vec![7u8; (w * h) as usize];
        // Cut off partway: the strips past the end can't be read.
        let bytes = grey8(w, h, &data, None);
        let cut = &bytes[..bytes.len() / 2];
        assert!(read_threaded(cut, max_samples()).is_err());
        assert!(read_threaded(cut, 100_000).is_err());
    }

    /// A tiled, uncompressed RGB GeoTIFF with a fourth, undefined sample (an RGBI orthophoto's
    /// layout), each image `(size, tile)` in turn: the first the full one, the rest overviews of
    /// it. Sample `b` of pixel (x, y) at a `k`-times overview is `pixel(b, x·k, y·k)`; edge tiles
    /// are padded with zeros.
    fn rgbi_tiled(images: &[(u32, u32)], pixel: impl Fn(u32, u32, u32) -> u8) -> Vec<u8> {
        let full = images.first().map_or(1, |i| i.0);
        let mut out = vec![b'I', b'I', 42, 0, 0, 0, 0, 0];
        let mut next_link = 4;
        for (i, &(size, tile)) in images.iter().enumerate() {
            let k = full / size;
            let n = size.div_ceil(tile);
            // The tiles' data first, then the IFD pointing at it.
            let mut offsets = Vec::new();
            for t in 0..n * n {
                offsets.push(out.len() as u32);
                for p in 0..tile * tile {
                    let (x, y) = ((t % n) * tile + p % tile, (t / n) * tile + p / tile);
                    for b in 0..4 {
                        out.push(if x < size && y < size { pixel(b, x * k, y * k) } else { 0 });
                    }
                }
            }
            // Out-of-line values: (tag, type, count, bytes); type 3 short, 4 long, 12 double.
            let shorts = |v: &[u16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
            let longs = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
            let doubles = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
            let counts = vec![tile * tile * 4; offsets.len()];
            let mut e: Vec<(u16, u16, u32, Vec<u8>)> = vec![
                (254, 4, 1, longs(&[u32::from(i > 0)])),
                (256, 4, 1, longs(&[size])),
                (257, 4, 1, longs(&[size])),
                (258, 3, 4, shorts(&[8; 4])),
                (259, 3, 1, shorts(&[1])),
                (262, 3, 1, shorts(&[2])),
                (277, 3, 1, shorts(&[4])),
                (284, 3, 1, shorts(&[1])),
                (322, 4, 1, longs(&[tile])),
                (323, 4, 1, longs(&[tile])),
                (324, 4, offsets.len() as u32, longs(&offsets)),
                (325, 4, counts.len() as u32, longs(&counts)),
                (338, 3, 1, shorts(&[0])),
            ];
            if i == 0 {
                e.push((33550, 12, 3, doubles(&[0.2, 0.2, 0.0])));
                e.push((33922, 12, 6, doubles(&[0.0, 0.0, 0.0, 641_000.0, 5_650_000.0, 0.0])));
                e.push((34735, 3, 8, shorts(&[1, 1, 0, 1, 3072, 0, 1, 25832])));
            }
            let mut values: Vec<u8> = Vec::new();
            let ifd_at = out.len() + out.len() % 2;
            out.resize(ifd_at, 0);
            let values_at = ifd_at + 2 + 12 * e.len() + 4;
            let mut dir = (e.len() as u16).to_le_bytes().to_vec();
            for (tag, ty, count, bytes) in &mut e {
                dir.extend(tag.to_le_bytes());
                dir.extend(ty.to_le_bytes());
                dir.extend(count.to_le_bytes());
                if bytes.len() <= 4 {
                    bytes.resize(4, 0);
                    dir.extend(bytes.iter());
                } else {
                    dir.extend(((values_at + values.len()) as u32).to_le_bytes());
                    values.extend(bytes.iter());
                }
            }
            out[next_link..next_link + 4].copy_from_slice(&(ifd_at as u32).to_le_bytes());
            next_link = ifd_at + dir.len();
            out.extend(dir);
            out.extend([0; 4]);
            out.extend(values);
        }
        out
    }

    #[test]
    fn padded_tiles_with_an_extra_sample_stay_in_their_rows() {
        // Issue #162: an RGBI orthophoto (5000 pixels in 128-pixel tiles) opened with black
        // columns down its west edge, the padding of each row's last tile spilt into the next row.
        let pixel = |b: u32, x: u32, y: u32| ((x + 3 * y + 60 * b) % 250 + 1) as u8;
        let bytes = rgbi_tiled(&[(40, 16), (20, 16)], pixel);
        let check = |r: &Raster, k: u32| {
            assert_eq!((r.band_count(), r.color.clone()), (3, ColorHint::Rgb { alpha: false }));
            for y in 0..r.height {
                for x in 0..r.width {
                    for b in 0..3 {
                        let want = f32::from(pixel(b, x * k, y * k));
                        assert_eq!(r.value(b as usize, i64::from(x), i64::from(y)), Some(want), "band {b} at {x},{y} (k {k})");
                    }
                }
            }
        };
        // The full image, and the overview a smaller limit (the browser's) opens instead.
        let r = read(std::io::Cursor::new(&bytes), "rgbi").unwrap();
        assert_eq!((r.width, r.height, r.overview), (40, 40, None));
        check(&r, 1);
        let r = read_limited(reader(std::io::Cursor::new(&bytes)).unwrap(), "rgbi", 2_000, None).unwrap();
        assert_eq!((r.width, r.height, r.overview), (20, 20, Some((40, 40))));
        check(&r, 2);
        assert_eq!(r.crs.epsg(), 25832);
        // With only a coarse overview (the orthophoto's is 157 pixels), every 2nd pixel of the
        // full image is read instead: four times the detail of the overview.
        let coarse = rgbi_tiled(&[(40, 16), (10, 16)], pixel);
        let r = read_limited(reader(std::io::Cursor::new(&coarse)).unwrap(), "rgbi", 2_000, None).unwrap();
        assert_eq!((r.width, r.height, r.overview), (20, 20, Some((40, 40))));
        check(&r, 2);
    }

    /// A `w` × `h` RGB image in `tw` × `th` tiles, uncompressed, its bands stored apart
    /// (planar configuration 2): every tile of band 0, then of band 1, then of band 2. The
    /// samples are bytes whatever `bits` says.
    fn planar_tiled(w: u32, h: u32, (tw, th): (u32, u32), bits: u16, pixel: impl Fn(u32, u32, u32) -> u8) -> Vec<u8> {
        let mut out = vec![b'I', b'I', 42, 0, 0, 0, 0, 0];
        let (across, down) = (w.div_ceil(tw), h.div_ceil(th));
        let mut offsets = Vec::new();
        for b in 0..3 {
            for t in 0..across * down {
                offsets.push(out.len() as u32);
                for p in 0..tw * th {
                    let (x, y) = ((t % across) * tw + p % tw, (t / across) * th + p / tw);
                    out.push(if x < w && y < h { pixel(b, x, y) } else { 0 });
                }
            }
        }
        let shorts = |v: &[u16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let longs = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let doubles = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let counts = vec![tw * th; offsets.len()];
        let mut e: Vec<(u16, u16, u32, Vec<u8>)> = vec![
            (256, 4, 1, longs(&[w])),
            (257, 4, 1, longs(&[h])),
            (258, 3, 3, shorts(&[bits; 3])),
            (259, 3, 1, shorts(&[1])),
            (262, 3, 1, shorts(&[2])),
            (277, 3, 1, shorts(&[3])),
            (284, 3, 1, shorts(&[2])),
            (322, 4, 1, longs(&[tw])),
            (323, 4, 1, longs(&[th])),
            (324, 4, offsets.len() as u32, longs(&offsets)),
            (325, 4, counts.len() as u32, longs(&counts)),
            (33550, 12, 3, doubles(&[30.0, 30.0, 0.0])),
            (33922, 12, 6, doubles(&[0.0, 0.0, 0.0, -8_300_000.0, -925_000.0, 0.0])),
            (34735, 3, 8, shorts(&[1, 1, 0, 1, 3072, 0, 1, 3857])),
        ];
        let ifd_at = out.len() + out.len() % 2;
        out.resize(ifd_at, 0);
        let values_at = ifd_at + 2 + 12 * e.len() + 4;
        let mut values: Vec<u8> = Vec::new();
        let mut dir = (e.len() as u16).to_le_bytes().to_vec();
        for (tag, ty, count, bytes) in &mut e {
            dir.extend(tag.to_le_bytes());
            dir.extend(ty.to_le_bytes());
            dir.extend(count.to_le_bytes());
            if bytes.len() <= 4 {
                bytes.resize(4, 0);
                dir.extend(bytes.iter());
            } else {
                dir.extend(((values_at + values.len()) as u32).to_le_bytes());
                values.extend(bytes.iter());
            }
        }
        out[4..8].copy_from_slice(&(ifd_at as u32).to_le_bytes());
        out.extend(dir);
        out.extend([0; 4]);
        out.extend(values);
        out
    }

    #[test]
    fn tiled_bands_stored_apart_are_read_whole() {
        // Issue #544: a tiled GeoTIFF with its bands stored apart (GDAL's INTERLEAVE=BAND), read
        // whole, panicked in the tiff crate on the padded bottom tiles of its later bands; read a
        // tile at a time (as a large file is), those tiles' rows came out skewed.
        let pixel = |b: u32, x: u32, y: u32| ((x + 3 * y + 60 * b) % 250 + 1) as u8;
        // Padded right, bottom, both and neither; a tile wider than tall, padded both ways.
        for (w, h, tile) in [(40, 24, (16, 16)), (32, 32, (16, 16)), (17, 9, (16, 16)), (24, 40, (16, 16)), (40, 20, (16, 8))] {
            let bytes = planar_tiled(w, h, tile, 8, pixel);
            // The whole image, and every 2nd pixel of it (a limit it doesn't fit).
            for (k, max) in [(1, max_samples()), (2, (w * h) as usize)] {
                let r = read_limited(reader(std::io::Cursor::new(&bytes)).unwrap(), "planar", max, None).unwrap();
                assert_eq!((r.width, r.height, r.band_count()), (w.div_ceil(k), h.div_ceil(k), 3));
                for y in 0..r.height {
                    for x in 0..r.width {
                        for b in 0..3 {
                            let want = f32::from(pixel(b, x * k, y * k));
                            let at = (i64::from(x), i64::from(y));
                            assert_eq!(r.value(b as usize, at.0, at.1), Some(want), "{w}×{h} (k {k}): band {b} at {x},{y}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn padded_planar_tiles_below_8_bits_are_refused() {
        // They can't be read a tile at a time, and the decoder panics reading them whole.
        let bytes = planar_tiled(40, 24, (16, 16), 4, |_, _, _| 0);
        let e = read(std::io::Cursor::new(&bytes), "planar").unwrap_err();
        assert!(e.to_string().contains("fewer than 8 bits"), "{e}");
        // Padded only on the right, they're left to the decoder (these samples are bytes, not
        // nibbles, so whatever it makes of them).
        let bytes = planar_tiled(40, 32, (16, 16), 4, |_, _, _| 0);
        let read = read(std::io::Cursor::new(&bytes), "planar");
        assert!(read.as_ref().err().is_none_or(|e| !e.to_string().contains("fewer than 8 bits")), "{read:?}");
    }

    #[test]
    fn band_descriptions_in_the_file_name_its_bands() {
        // GDAL and rasterio keep band descriptions in the GDAL_METADATA tag (42112).
        use tiff::encoder::{TiffEncoder, colortype};
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut enc = TiffEncoder::new(&mut out).unwrap();
            let mut img = enc.new_image::<colortype::Gray32Float>(2, 2).unwrap();
            let e = img.encoder();
            e.write_tag(Tag::ModelPixelScaleTag, &[0.5f64, 0.5, 0.0][..]).unwrap();
            e.write_tag(Tag::ModelTiepointTag, &[0.0f64, 0.0, 0.0, 10.0, 50.0, 0.0][..]).unwrap();
            e.write_tag(Tag::GeoKeyDirectoryTag, &[1u16, 1, 0, 2, 1024, 0, 1, 2, 2048, 0, 1, 4326][..]).unwrap();
            let xml = r#"<GDALMetadata><Item name="DESCRIPTION" sample="0" role="description">NDVI 2024-06-01</Item></GDALMetadata>"#;
            e.write_tag(Tag::Unknown(42112), xml).unwrap();
            img.write_data(&[0.1f32, 0.2, 0.3, 0.4]).unwrap();
        }
        let r = read(std::io::Cursor::new(out.into_inner()), "ndvi").unwrap();
        assert_eq!(r.bands[0].name, "NDVI 2024-06-01");
        // Without the tag, the band keeps its number.
        let plain = read(std::io::Cursor::new(grey8(2, 2, &[1, 2, 3, 4], None)), "plain").unwrap();
        assert_ne!(plain.bands[0].name, "NDVI 2024-06-01");
    }

    #[test]
    fn what_a_files_metadata_leaves_out_is_kept_to_be_said() {
        use crate::raster::cog::{Example, example_with};
        // Read whole, a file with a class item past a band's values says so; a clean one doesn't.
        let lossy = read(
            std::io::Cursor::new(example_with(&Example { size: 256, tile: 256, classes: true, lossy: true, ..Example::default() })),
            "lossy",
        )
        .unwrap();
        assert!(lossy.lost.iter().any(|l| l.starts_with("its GDAL metadata: 1 CLASS_ item(s)")), "{:?}", lossy.lost);
        let clean =
            read(std::io::Cursor::new(example_with(&Example { size: 256, tile: 256, classes: true, ..Example::default() })), "clean")
                .unwrap();
        assert!(clean.lost.is_empty(), "{:?}", clean.lost);
    }

    #[test]
    fn class_names_are_written_back() {
        // In GDAL's metadata tag, as a window of MapBiomas keeps its classes in a saved project.
        let mut r = read(std::io::Cursor::new(grey8(2, 2, &[3, 15, 3, 0], None)), "cover").unwrap();
        r.bands[0].classes = vec![String::new(); 16];
        r.bands[0].classes[3] = "Formação Florestal".into();
        r.bands[0].classes[15] = "Pastagem".into();
        r.bands[0].name = "classe_mapbiomas".into();
        let back = read(std::io::Cursor::new(write(&r, 0).unwrap()), "back").unwrap();
        assert_eq!((back.bands[0].class_name(3.0), back.bands[0].class_name(15.0)), (Some("Formação Florestal"), Some("Pastagem")));
        assert_eq!(back.bands[0].name, "classe_mapbiomas");
        // The same through the cloud-optimised writer.
        let cog = read(std::io::Cursor::new(super::super::cogwrite::write(&r, 0).unwrap()), "cog").unwrap();
        assert_eq!((cog.bands[0].name.as_str(), cog.bands[0].class_name(3.0)), ("classe_mapbiomas", Some("Formação Florestal")));
    }

    #[test]
    fn a_files_own_projection_is_read_and_written_back() {
        // The NLCD's Albers, as its GeoKeys give it.
        let keys = [
            1u16, 1, 0, 11, 1024, 0, 1, 1, 1025, 0, 1, 1, 2048, 0, 1, 4326, 3072, 0, 1, 32767, 3074, 0, 1, 32767, 3075, 0, 1, 11, 3076, 0,
            1, 9001, 3078, 34736, 1, 0, 3079, 34736, 1, 1, 3080, 34736, 1, 2, 3081, 34736, 1, 3, 3082, 34736, 1, 4,
        ];
        let doubles = [29.5f64, 45.5, -96.0, 23.0, 0.0];
        let mut tiff = Vec::new();
        {
            use tiff::encoder::{TiffEncoder, colortype};
            let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut tiff)).unwrap();
            let mut img = enc.new_image::<colortype::Gray32Float>(2, 2).unwrap();
            let e = img.encoder();
            e.write_tag(Tag::ModelPixelScaleTag, &[30.0f64, 30.0, 0.0][..]).unwrap();
            e.write_tag(Tag::ModelTiepointTag, &[0.0f64, 0.0, 0.0, 0.0, 60.0, 0.0][..]).unwrap();
            e.write_tag(Tag::GeoKeyDirectoryTag, &keys[..]).unwrap();
            e.write_tag(Tag::Unknown(34736), &doubles[..]).unwrap();
            img.write_data(&[1.0f32, 2.0, 3.0, 4.0]).unwrap();
        }
        let r = read(std::io::Cursor::new(&tiff), "albers").unwrap();
        assert!(matches!(r.crs, Crs::Custom(_)), "{:?}", r.crs);
        assert!(r.crs.custom_def().unwrap().proj4.contains("+proj=aea"));
        // The projection's origin is at 96° W, 23° N.
        let ll = r.crs.transformer().unwrap().to_lonlat(rustgis_geom::Coord::new(0.0, 0.0)).unwrap();
        assert!((ll.x + 96.0).abs() < 1e-6 && (ll.y - 23.0).abs() < 1e-6, "{ll:?}");
        // Written and read again, the same system.
        let back = read(std::io::Cursor::new(write(&r, 0).unwrap()), "back").unwrap();
        assert_eq!(back.crs.custom_def().unwrap().proj4, r.crs.custom_def().unwrap().proj4);
        assert_eq!(back.value(0, 1, 1), Some(4.0));
        // Saved, it is its definition, not this run's number for it.
        let json = serde_json::to_string(&r.crs).unwrap();
        assert!(json.contains("+proj=aea"), "{json}");
        let again: Crs = serde_json::from_str(&json).unwrap();
        assert_eq!(again.custom_def().unwrap().proj4, r.crs.custom_def().unwrap().proj4);
        // False eastings in US feet become metres for PROJ.
        let mut feet = keys.to_vec();
        if let Some(i) = feet.chunks(4).position(|e| e[0] == 3076) {
            feet[i * 4 + 3] = 9003;
        }
        let ft = custom_crs(&feet, &[29.5, 45.5, -96.0, 23.0, 1000.0], "").unwrap();
        let p = ft.custom_def().unwrap().proj4.clone();
        assert!(p.contains("+x_0=304.80060960") && p.contains("+units=us-ft"), "{p}");
        // A transformation it doesn't know: unknown, not a failed open, when the coordinates are degrees.
        let mut odd = keys;
        odd[27] = 99; // ProjCoordTransGeoKey
        assert!(custom_crs(&odd, &doubles, "").is_err());
    }

    /// MODIS's sinusoidal grid (coordinate transformation 24), as a MOD13Q1 tile's GeoKeys give
    /// it: h12v10, whose corner is at 60.925597° W, 10° S (pyproj 3.7.1).
    #[test]
    fn modis_sinusoidal_is_read_and_written_back() {
        let keys = [
            1u16, 1, 0, 14, 1024, 0, 1, 1, 1025, 0, 1, 1, 2048, 0, 1, 32767, 2050, 0, 1, 32767, 2054, 0, 1, 9102, 2056, 0, 1, 32767, 2057,
            34736, 1, 0, 2058, 34736, 1, 1, 3072, 0, 1, 32767, 3074, 0, 1, 32767, 3075, 0, 1, 24, 3076, 0, 1, 9001, 3082, 34736, 1, 2,
            3083, 34736, 1, 3, 3088, 34736, 1, 4,
        ];
        let doubles = [6_371_007.181f64, 6_371_007.181, 0.0, 0.0, 0.0];
        let px = 231.656_358_263_958_25;
        let mut tiff = Vec::new();
        {
            use tiff::encoder::{TiffEncoder, colortype};
            let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut tiff)).unwrap();
            let mut img = enc.new_image::<colortype::Gray32Float>(2, 2).unwrap();
            let e = img.encoder();
            e.write_tag(Tag::ModelPixelScaleTag, &[px, px, 0.0][..]).unwrap();
            e.write_tag(Tag::ModelTiepointTag, &[0.0f64, 0.0, 0.0, -6_671_703.118, -1_111_950.519_667, 0.0][..]).unwrap();
            e.write_tag(Tag::GeoKeyDirectoryTag, &keys[..]).unwrap();
            e.write_tag(Tag::Unknown(34736), &doubles[..]).unwrap();
            img.write_data(&[1.0f32, 2.0, 3.0, 4.0]).unwrap();
        }
        let r = read(std::io::Cursor::new(&tiff), "modis").unwrap();
        let p = r.crs.custom_def().unwrap().proj4.clone();
        assert!(p.contains("+proj=sinu") && p.contains("+a=6371007.181 +b=6371007.181"), "{p}");
        let t = r.crs.transformer().unwrap();
        let ll = t.to_lonlat(rustgis_geom::Coord::new(-6_671_703.118, -1_111_950.519_667)).unwrap();
        assert!((ll.x + 60.925_597).abs() < 1e-6 && (ll.y + 10.0).abs() < 1e-9, "{ll:?}");
        let m = t.from_lonlat(ll).unwrap();
        assert!((m.x + 6_671_703.118).abs() < 1e-3 && (m.y + 1_111_950.519_667).abs() < 1e-3, "{m:?}");
        // Written and read again, the same system; saved, its definition.
        let back = read(std::io::Cursor::new(write(&r, 0).unwrap()), "back").unwrap();
        assert_eq!(back.crs.custom_def().unwrap().proj4, p);
        let again: Crs = serde_json::from_str(&serde_json::to_string(&r.crs).unwrap()).unwrap();
        assert_eq!(again.custom_def().unwrap().proj4, p);
        // ESRI:54008's definition, on WGS 84, is written as keys too.
        let (k, d, a) = keys_from_proj4("+proj=sinu +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs", "World_Sinusoidal").unwrap();
        assert!(custom_crs(&k, &d, &a).unwrap().custom_def().unwrap().proj4.starts_with("+proj=sinu"));
        // A central meridian and false origin away from 0 (ESRI:102011, Africa Sinusoidal, at 15°)
        // go through the keys and back as the same projection.
        let africa = "+proj=sinu +lon_0=15 +x_0=200000 +y_0=-100000 +datum=WGS84 +units=m +no_defs";
        let (k, d, a) = keys_from_proj4(africa, "Africa_Sinusoidal").unwrap();
        let back = custom_crs(&k, &d, &a).unwrap().custom_def().unwrap().proj4.clone();
        assert!(same_projection(africa, &back), "{back}");
        // A false easting or northing of its own is another projection.
        assert!(!same_projection(africa, "+proj=sinu +lon_0=15 +x_0=0 +y_0=-100000 +datum=WGS84 +units=m +no_defs"));
        assert!(!same_projection(africa, "+proj=sinu +lon_0=15 +x_0=200000 +y_0=0 +datum=WGS84 +units=m +no_defs"));
    }

    /// A tiled, LZW-compressed Float32 GeoTIFF, `w × h` in `tile`-pixel tiles, written as GDAL
    /// writes one: each tile whole (padding zeros) through `predictor` (1, 2 or 3) and LZW.
    fn lzw_tiled(w: u32, h: u32, tile: u32, predictor: u16, pixel: impl Fn(u32, u32) -> f32) -> Vec<u8> {
        let (across, down) = (w.div_ceil(tile), h.div_ceil(tile));
        let mut out = vec![b'I', b'I', 42, 0, 0, 0, 0, 0];
        let (mut offsets, mut counts) = (Vec::new(), Vec::new());
        for t in 0..across * down {
            let (x0, y0) = ((t % across) * tile, (t / across) * tile);
            let mut raw = Vec::new();
            for y in y0..y0 + tile {
                let row: Vec<f32> = (x0..x0 + tile).map(|x| if x < w && y < h { pixel(x, y) } else { 0.0 }).collect();
                match predictor {
                    1 => raw.extend(row.iter().flat_map(|v| v.to_le_bytes())),
                    2 => raw.extend(
                        row.iter()
                            .enumerate()
                            .flat_map(|(i, v)| v.to_bits().wrapping_sub(if i == 0 { 0 } else { row[i - 1].to_bits() }).to_le_bytes()),
                    ),
                    _ => {
                        let n = row.len();
                        let mut b = vec![0u8; n * 4];
                        for (i, v) in row.iter().enumerate() {
                            for (k, byte) in v.to_bits().to_be_bytes().into_iter().enumerate() {
                                b[k * n + i] = byte;
                            }
                        }
                        for i in (1..b.len()).rev() {
                            b[i] = b[i].wrapping_sub(b[i - 1]);
                        }
                        raw.extend(b);
                    }
                }
            }
            let lzw = weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8).encode(&raw).unwrap();
            offsets.push(out.len() as u32);
            counts.push(lzw.len() as u32);
            out.extend(lzw);
        }
        let shorts = |v: &[u16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let longs = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let doubles = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let mut e: Vec<(u16, u16, u32, Vec<u8>)> = vec![
            (256, 4, 1, longs(&[w])),
            (257, 4, 1, longs(&[h])),
            (258, 3, 1, shorts(&[32])),
            (259, 3, 1, shorts(&[5])),
            (262, 3, 1, shorts(&[1])),
            (277, 3, 1, shorts(&[1])),
            (317, 3, 1, shorts(&[predictor])),
            (322, 4, 1, longs(&[tile])),
            (323, 4, 1, longs(&[tile])),
            (324, 4, offsets.len() as u32, longs(&offsets)),
            (325, 4, counts.len() as u32, longs(&counts)),
            (339, 3, 1, shorts(&[3])),
            (33550, 12, 3, doubles(&[10.0, 10.0, 0.0])),
            (33922, 12, 6, doubles(&[0.0, 0.0, 0.0, 660_000.0, 5_115_000.0, 0.0])),
            (34735, 3, 8, shorts(&[1, 1, 0, 1, 3072, 0, 1, 25832])),
        ];
        let ifd_at = out.len() + out.len() % 2;
        out.resize(ifd_at, 0);
        out[4..8].copy_from_slice(&(ifd_at as u32).to_le_bytes());
        let values_at = ifd_at + 2 + 12 * e.len() + 4;
        let (mut dir, mut values) = ((e.len() as u16).to_le_bytes().to_vec(), Vec::<u8>::new());
        for (tag, ty, count, bytes) in &mut e {
            dir.extend(tag.to_le_bytes());
            dir.extend(ty.to_le_bytes());
            dir.extend(count.to_le_bytes());
            if bytes.len() <= 4 {
                bytes.resize(4, 0);
                dir.extend(bytes.iter());
            } else {
                dir.extend(((values_at + values.len()) as u32).to_le_bytes());
                values.extend(bytes.iter());
            }
        }
        out.extend(dir);
        out.extend([0; 4]);
        out.extend(values);
        out
    }

    /// An elevation model's tile: mostly no data, the rest small whole numbers, from `seed`.
    fn dtm_tile(seed: u64, tile: u32) -> Vec<f32> {
        let next = |s: u64| s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut s = next(seed);
        (0..tile * tile)
            .map(|_| {
                s = next(s);
                if (s >> 60) < 12 { -3.4028235e38 } else { ((s >> 40) % 8) as f32 }
            })
            .collect()
    }

    #[test]
    fn lzw_tiles_the_tiff_crate_drops_bytes_of_are_read() {
        // Issue #350: a 104-pixel-wide Float32 DTM in 128-pixel LZW tiles failed with "no lzw end
        // code found". The tiff crate drops a padded tile's last bytes when the LZW decoder gives
        // them back with the input used up, which depends on the data: this tile does that.
        let values = dtm_tile(40, 128);
        let pixel = |x: u32, y: u32| values[(y * 128 + x) as usize];
        let bytes = lzw_tiled(104, 128, 128, 1, pixel);
        let mut dec = Decoder::new(std::io::Cursor::new(&bytes)).unwrap();
        assert!(dec.read_chunk(0).is_err(), "the tiff crate reads this tile now: the test no longer shows issue #350");
        let check = |r: &Raster| {
            assert_eq!((r.width, r.height, r.data_type.as_str()), (104, 128, "Float32"));
            for (x, y) in [(0, 0), (103, 0), (50, 64), (0, 127), (103, 127)] {
                assert_eq!(r.bands[0].data[(y * 104 + x) as usize].to_bits(), pixel(x, y).to_bits(), "pixel ({x}, {y})");
            }
            assert!((0..104 * 128).all(|i| r.bands[0].data[i as usize].to_bits() == pixel(i % 104, i / 104).to_bits()));
        };
        check(&read(std::io::Cursor::new(&bytes), "dtm").unwrap());
        // From a file, a chunk at a time; and in the browser, in parts.
        check(&read_threaded(&bytes, max_samples()).unwrap());
        check(&read_in_parts(&bytes, 512, max_samples()).0);
        // The same tile of a larger image, as one of the tiles a COG is shown with.
        let wide = lzw_tiled(232, 200, 128, 1, |x, y| if x < 128 && y < 128 { pixel(x, y) } else { (x + y) as f32 });
        let r = read(std::io::Cursor::new(&wide), "wide").unwrap();
        assert_eq!(r.bands[0].data[231 + 199 * 232], 430.0);
        assert_eq!(r.bands[0].data[5 * 232 + 7].to_bits(), pixel(7, 5).to_bits());
        // A damaged tile fails the open, never the app: cut short, garbled.
        let ifd = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let mut cut = bytes.clone();
        // Its TileByteCounts (one value, in its IFD entry) halved.
        let entries = ifd + 2;
        let at = (0..15).map(|i| entries + 12 * i).find(|&e| cut[e..e + 2] == 325u16.to_le_bytes()).unwrap() + 8;
        let count = u32::from_le_bytes(cut[at..at + 4].try_into().unwrap());
        cut[at..at + 4].copy_from_slice(&(count / 2).to_le_bytes());
        assert!(read(std::io::Cursor::new(&cut), "cut").is_err());
        for seed in 0..64u64 {
            let mut junk = bytes.clone();
            for (i, b) in junk[8..ifd].iter_mut().enumerate() {
                *b ^= ((seed.wrapping_mul(2654435761) >> (i % 23)) as u8) & 0x5a;
            }
            let _ = read(std::io::Cursor::new(&junk), "junk");
        }
    }

    /// One image of [`ycbcr_jpeg_tiff`] to put at byte `base`: `w` × `h` (`w`, `h` and `rows` multiples
    /// of 16) of four flat colours, one per quarter, each strip of `rows` rows a JPEG, as GDAL writes
    /// a `COMPRESS=JPEG PHOTOMETRIC=YCBCR` file: JPEG compression, YCbCr photometric, chroma
    /// subsampled 2×1 (as the `image` crate's encoder does; GDAL's 2×2 was checked by hand on a
    /// GDAL-made file). With `geo` it has the GeoTIFF tags, as `overview` the reduced-image flag.
    /// Returns its bytes and where its next-IFD offset is.
    fn ycbcr_jpeg_image(base: usize, (w, h, rows): (u32, u32, u32), colors: [[u8; 3]; 4], geo: bool, overview: bool) -> (Vec<u8>, usize) {
        let strips: Vec<Vec<u8>> = (0..h / rows)
            .map(|s| {
                let mut rgb = image::RgbImage::new(w, rows);
                for (x, y, px) in rgb.enumerate_pixels_mut() {
                    *px = image::Rgb(colors[(x / (w / 2) + 2 * ((s * rows + y) / (h / 2))) as usize]);
                }
                let mut jpeg = Vec::new();
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 100).encode_image(&rgb).unwrap();
                jpeg
            })
            .collect();
        let n = strips.len() as u32;
        let le16 = |v: &[u16]| v.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
        let le32 = |v: &[u32]| v.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
        let le64 = |v: &[f64]| v.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
        // (tag, type, count, bytes): values of up to four bytes go in the entry, longer ones after
        // the entries (the strip offsets are filled in once the entries' size is known).
        let mut entries: Vec<(u16, u16, u32, Vec<u8>)> = Vec::new();
        if overview {
            entries.push((254, 4, 1, le32(&[1])));
        }
        entries.extend([
            (256, 3, 1, le16(&[w as u16])),
            (257, 3, 1, le16(&[h as u16])),
            (258, 3, 3, le16(&[8, 8, 8])),
            (259, 3, 1, le16(&[7])),
            (262, 3, 1, le16(&[6])),
            (273, 4, n, vec![0; 4 * n as usize]),
            (277, 3, 1, le16(&[3])),
            (278, 3, 1, le16(&[rows as u16])),
            (279, 4, n, le32(&strips.iter().map(|s| s.len() as u32).collect::<Vec<_>>())),
            (284, 3, 1, le16(&[1])),
            (530, 3, 2, le16(&[2, 1])),
            // ReferenceBlackWhite, the JFIF values, as rationals.
            (532, 5, 6, le32(&[0, 1, 255, 1, 128, 1, 255, 1, 128, 1, 255, 1])),
        ]);
        if geo {
            entries.push((33550, 12, 3, le64(&[1.0, 1.0, 0.0])));
            entries.push((33922, 12, 6, le64(&[0.0, 0.0, 0.0, 500_000.0, 5_000_000.0, 0.0])));
            entries.push((34735, 3, 16, le16(&[1, 1, 0, 3, 1024, 0, 1, 1, 1025, 0, 1, 1, 3072, 0, 1, 25832])));
        }
        let table_end = base + 2 + 12 * entries.len() + 4;
        let long: usize = entries.iter().filter(|e| e.3.len() > 4).map(|e| e.3.len()).sum();
        let mut at = table_end;
        let mut strip_at = (table_end + long) as u32;
        let (mut table, mut data) = (Vec::new(), Vec::new());
        for (tag, kind, count, bytes) in &entries {
            let mut bytes = bytes.clone();
            if *tag == 273 {
                bytes = le32(&strips.iter().map(|s| (strip_at, strip_at += s.len() as u32).0).collect::<Vec<_>>());
            }
            table.extend(tag.to_le_bytes());
            table.extend(kind.to_le_bytes());
            table.extend(count.to_le_bytes());
            if bytes.len() > 4 {
                table.extend((at as u32).to_le_bytes());
                at += bytes.len();
                data.extend(bytes);
            } else {
                bytes.resize(4, 0);
                table.extend(bytes);
            }
        }
        let mut out = (entries.len() as u16).to_le_bytes().to_vec();
        out.extend(table);
        let next = 2 + 12 * entries.len();
        out.extend(0u32.to_le_bytes());
        out.extend(data);
        out.extend(strips.concat());
        (out, next)
    }

    /// A TIFF of [`ycbcr_jpeg_image`]s: the full image with its GeoTIFF tags (unless `!geo`), then
    /// any overviews.
    fn ycbcr_jpeg_tiff(sizes: &[(u32, u32, u32)], colors: [[u8; 3]; 4], geo: bool) -> Vec<u8> {
        let mut out = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        let mut next_at = None;
        for (i, size) in sizes.iter().enumerate() {
            let base = out.len();
            if let Some(at) = next_at {
                out[at..at + 4].copy_from_slice(&(base as u32).to_le_bytes());
            }
            let (bytes, next) = ycbcr_jpeg_image(base, *size, colors, geo && i == 0, i > 0);
            next_at = Some(base + next);
            out.extend(bytes);
        }
        out
    }

    #[test]
    fn ycbcr_jpeg_is_shown_as_rgb() {
        // Issue #692: the tiff crate leaves JPEG-compressed YCbCr as it is, which drew a COG with
        // a purple and green cast. Flat colours survive the JPEG within a few levels.
        let colors = [[200, 30, 30], [30, 160, 40], [20, 40, 220], [230, 230, 60]];
        let check = |r: &Raster, w: usize, h: usize| {
            assert_eq!((r.width as usize, r.height as usize, r.bands.len(), r.color.clone()), (w, h, 3, ColorHint::Rgb { alpha: false }));
            for (i, rgb) in colors.iter().enumerate() {
                let (x, y) = (w / 4 + w / 2 * (i % 2), h / 4 + h / 2 * (i / 2));
                for (band, want) in r.bands.iter().zip(rgb) {
                    let got = band.data[y * w + x];
                    assert!((got - f32::from(*want)).abs() <= 4.0, "quarter {i}: {got} for {want}");
                }
            }
        };
        let bytes = ycbcr_jpeg_tiff(&[(32, 32, 32)], colors, true);
        let mut dec = Decoder::new(std::io::Cursor::new(&bytes)).unwrap();
        assert!(matches!(dec.colortype().unwrap(), ColorType::YCbCr(8)));
        check(&read(std::io::Cursor::new(&bytes), "ycc").unwrap(), 32, 32);
        // Without geo tags (a scan), which returns before the georeferencing.
        let scan = read(std::io::Cursor::new(ycbcr_jpeg_tiff(&[(32, 32, 32)], colors, false)), "scan").unwrap();
        assert!(!scan.georeferenced);
        check(&scan, 32, 32);
        // A million pixels in 16 strips, read a chunk at a time on several threads.
        let big = ycbcr_jpeg_tiff(&[(1024, 1024, 64)], colors, true);
        check(&read_threaded(&big, max_samples()).unwrap(), 1024, 1024);
        // An image too large for the limit is shown by its overview, a JPEG of its own.
        let cog = ycbcr_jpeg_tiff(&[(64, 64, 64), (32, 32, 32)], colors, true);
        let r = read_limited(reader(std::io::Cursor::new(&cog)).unwrap(), "cog", 4_000, None).unwrap();
        assert_eq!(r.overview, Some((64, 64)));
        check(&r, 32, 32);
    }

    #[test]
    fn lzw_chunks_decoded_here_match_the_tiff_crate() {
        // Every predictor, padded edge tiles on the right and at the bottom.
        let pixel = |x: u32, y: u32| (x as f32 * 0.37 - y as f32 * 1.5).sin() * 900.0 + 1200.0;
        for predictor in [1, 2, 3] {
            let bytes = lzw_tiled(300, 170, 128, predictor, pixel);
            let mut dec = Decoder::new(std::io::Cursor::new(&bytes)).unwrap();
            for i in 0..6 {
                let (Ok(DecodingResult::F32(theirs)), Ok(DecodingResult::F32(ours))) =
                    (dec.read_chunk(i), super::super::lzw::chunk(&mut dec, i))
                else {
                    panic!("predictor {predictor}, tile {i}: not Float32");
                };
                assert_eq!(ours, theirs, "predictor {predictor}, tile {i}");
            }
            let r = read(std::io::Cursor::new(&bytes), "p").unwrap();
            assert_eq!(r.bands[0].data[169 * 300 + 299], pixel(299, 169));
        }
        // Integer samples in strips, several to a pixel, with the horizontal predictor.
        use tiff::encoder::{Compression, Predictor, TiffEncoder, colortype};
        let (w, h) = (37u32, 23u32);
        let rgb: Vec<u16> = (0..w * h * 3).map(|i| ((i * 7919) % 65521) as u16).collect();
        let grey: Vec<i32> = (0..w * h).map(|i| (i as i32 - 400) * 104_729).collect();
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut enc = TiffEncoder::new(&mut out).unwrap().with_compression(Compression::Lzw).with_predictor(Predictor::Horizontal);
            let mut img = enc.new_image::<colortype::RGB16>(w, h).unwrap();
            img.rows_per_strip(5).unwrap();
            img.write_data(&rgb).unwrap();
            let mut img = enc.new_image::<colortype::GrayI32>(w, h).unwrap();
            img.rows_per_strip(6).unwrap();
            img.write_data(&grey).unwrap();
        }
        let bytes = out.into_inner();
        let mut dec = Decoder::new(std::io::Cursor::new(&bytes)).unwrap();
        for image in 0..2 {
            dec.seek_to_image(image).unwrap();
            for i in 0..dec.strip_count().unwrap() {
                let (theirs, ours) = (dec.read_chunk(i).unwrap(), super::super::lzw::chunk(&mut dec, i).unwrap());
                assert_eq!(to_f32(ours, (1, 1)).unwrap(), to_f32(theirs, (1, 1)).unwrap(), "image {image}, strip {i}");
            }
        }
    }

    #[test]
    fn custom_crs_geotiff_round_trip() {
        let r = small();
        let custom_proj =
            Crs::parse("+proj=tmerc +lat_0=0 +lon_0=105 +k=0.9999 +x_0=500000 +y_0=0 +datum=WGS84 +units=m +no_defs").unwrap();
        let raster_custom = Raster::new(
            "cust",
            r.width,
            r.height,
            r.bands.iter().map(|b| b.data.clone()).collect(),
            r.transform,
            custom_proj,
            r.nodata,
            r.color.clone(),
            "Float32",
        )
        .unwrap();
        let bytes = write(&raster_custom, 0).expect("custom CRS with standard projection parameters should serialize to GeoTIFF");
        let back = read(std::io::Cursor::new(&bytes), "back").unwrap();
        assert_eq!((back.width, back.height), (r.width, r.height));
        assert_eq!(back.transform, r.transform);
        assert!(!back.crs.is_geographic());
        // Verify custom CRS definition is equivalent and usable
        let def = back.crs.custom_def().expect("re-opened raster has custom CRS definition");
        assert!(def.proj4.contains("+proj=tmerc"), "{}", def.proj4);
        assert!(def.proj4.contains("+lon_0=105"), "{}", def.proj4);
        let tr = back.crs.transformer().expect("transformer available");
        let pt = rustgis_geom::Coord::new(105.5, 21.0);
        let projected = tr.from_lonlat(pt).unwrap();
        let back_pt = tr.to_lonlat(projected).unwrap();
        assert!((back_pt.x - pt.x).abs() < 1e-6 && (back_pt.y - pt.y).abs() < 1e-6);

        // VN-2000 / TM-3 is EPSG:5897, written as its code and read back as it.
        let vn2000_tm3 = Crs::parse("VN-2000 / TM-3").unwrap();
        let raster_vn2000 = Raster::new(
            "vn",
            r.width,
            r.height,
            r.bands.iter().map(|b| b.data.clone()).collect(),
            r.transform,
            vn2000_tm3,
            r.nodata,
            r.color,
            "Float32",
        )
        .unwrap();
        let bytes = write(&raster_vn2000, 0).unwrap();
        let back = read(std::io::Cursor::new(&bytes), "vn").unwrap();
        assert_eq!(back.crs, Crs::Epsg(5897));
    }

    /// A longitude/latitude GeoTIFF of `w` × `h` pixels of colour type `C`, a row to a strip, with
    /// GDAL's no-data tag when `nodata` is given.
    fn typed<C: tiff::encoder::colortype::ColorType>(w: u32, h: u32, data: &[C::Inner], nodata: Option<&str>) -> Vec<u8>
    where
        [C::Inner]: tiff::encoder::TiffValue,
    {
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut enc = tiff::encoder::TiffEncoder::new(&mut out).unwrap();
            let mut img = enc.new_image::<C>(w, h).unwrap();
            img.rows_per_strip(1).unwrap();
            let e = img.encoder();
            e.write_tag(Tag::ModelPixelScaleTag, &[0.1f64, 0.1, 0.0][..]).unwrap();
            e.write_tag(Tag::ModelTiepointTag, &[0.0f64, 0.0, 0.0, 10.0, 46.0, 0.0][..]).unwrap();
            e.write_tag(Tag::GeoKeyDirectoryTag, &[1u16, 1, 0, 2, 1024, 0, 1, 2, 2048, 0, 1, 4326][..]).unwrap();
            if let Some(nd) = nodata {
                e.write_tag(Tag::GdalNodata, nd).unwrap();
            }
            img.write_data(data).unwrap();
        }
        out.into_inner()
    }

    #[test]
    fn values_f32_cannot_hold_are_counted() {
        use tiff::encoder::colortype::{Gray64Float, GrayI32, GrayI64, RGB32};
        // Int32 past 16,777,216: 16777217 and 100000001 change, 16777216 itself doesn't.
        let ids = read(std::io::Cursor::new(typed::<GrayI32>(2, 2, &[16_777_217, 16_777_216, 100_000_001, 7], None)), "ids").unwrap();
        assert_eq!((ids.data_type.as_str(), ids.rounded.as_slice()), ("Int32", &[2u64][..]));
        assert_eq!(ids.bands[0].data[2], 100_000_000.0);
        let clean = read(std::io::Cursor::new(typed::<GrayI32>(2, 2, &[16_777_216, -16_777_216, 12_345, 7], None)), "c").unwrap();
        assert!(clean.rounded.is_empty());
        // Float64 with more digits than f32 keeps; NaN and the no-data value (past f32's range)
        // aren't counted.
        let f = [0.123_456_789_012_345, f64::MIN, 0.5, f64::NAN];
        let r = read(std::io::Cursor::new(typed::<Gray64Float>(2, 2, &f, Some("-1.7976931348623157e+308"))), "f").unwrap();
        assert_eq!(r.rounded, vec![1]);
        let exact = read(std::io::Cursor::new(typed::<Gray64Float>(2, 2, &[0.5, 2.0, -3.25, 1e6], None)), "e").unwrap();
        assert!(exact.rounded.is_empty());
        // Int64 past 2^53: only the no-data value itself is left out, not one an f64 can't tell from it.
        let ids = typed::<GrayI64>(2, 2, &[i64::MIN, i64::MIN + 1, 0, 1], Some("-9223372036854775808"));
        assert_eq!(read(std::io::Cursor::new(ids), "ids").unwrap().rounded, vec![1]);
        // A value that rounds onto the no-data value becomes no-data, so it is counted; the no-data
        // value itself isn't.
        let onto = read(std::io::Cursor::new(typed::<Gray64Float>(2, 2, &[1.000_000_01, 1.0, 0.5, 2.0], Some("1"))), "o").unwrap();
        assert_eq!(onto.rounded, vec![1]);
        // A no-data value only a sidecar gives isn't counted: 0.1 isn't, as no-data, but the f32
        // nearest it, a value read exactly that the sidecar's value now masks, is, and so is
        // 0.123456789012345.
        // A directory of its own, removed even when an assertion fails, so no sidecar is left over.
        let dir = tempfile::tempdir().unwrap();
        let tif = dir.path().join("s.tif");
        let px = [0.1, f64::from(0.1f32), 0.123_456_789_012_345, 2.0];
        std::fs::write(&tif, typed::<Gray64Float>(2, 2, &px, None)).unwrap();
        assert_eq!(crate::raster::read_path(&tif).unwrap().rounded, vec![2]);
        let xml = r#"<PAMDataset><PAMRasterBand band="1"><NoDataValue>0.1</NoDataValue></PAMRasterBand></PAMDataset>"#;
        std::fs::write(dir.path().join("s.tif.aux.xml"), xml).unwrap();
        let side = crate::raster::read_path(&tif).unwrap();
        assert_eq!((side.nodata, side.rounded.as_slice()), (Some(0.1), &[2u64][..]));
        // The same with a NaN no-data tag, which leaves the sidecar's value in force.
        std::fs::write(&tif, typed::<Gray64Float>(2, 2, &px, Some("nan"))).unwrap();
        let nan = crate::raster::read_path(&tif).unwrap();
        assert_eq!((nan.nodata, nan.rounded.as_slice()), (Some(0.1), &[2u64][..]));
        // A record for a band the file lacks, with another no-data value, doesn't stop band 1's
        // from applying, nor from being left out of the count: 0.1 isn't counted, 0.1234… is.
        let px = [0.1, 0.5, 0.123_456_789_012_345, 2.0];
        std::fs::write(&tif, typed::<Gray64Float>(2, 2, &px, None)).unwrap();
        let xml = r#"<PAMDataset><PAMRasterBand band="1"><NoDataValue>0.1</NoDataValue></PAMRasterBand>
            <PAMRasterBand band="2"><NoDataValue>5</NoDataValue></PAMRasterBand></PAMDataset>"#;
        std::fs::write(dir.path().join("s.tif.aux.xml"), xml).unwrap();
        let other = crate::raster::read_path(&tif).unwrap();
        assert_eq!((other.nodata, other.rounded.as_slice()), (Some(0.1), &[1u64][..]), "{:?}", other.lost);
        // 64-bit whole numbers are compared as integers: 2^60 + 1 changes, 2^60 doesn't.
        let big = read(std::io::Cursor::new(typed::<GrayI64>(2, 1, &[(1 << 60) + 1, 1 << 60], None)), "i64").unwrap();
        assert_eq!(big.rounded, vec![1]);
        // Interleaved bands are counted apart.
        let rgb = read(std::io::Cursor::new(typed::<RGB32>(2, 1, &[1, 20_000_001, 3, 4, 5, 6], None)), "rgb").unwrap();
        assert_eq!(rgb.rounded, vec![0, 1, 0]);
    }

    #[test]
    fn a_reduced_read_counts_only_the_pixels_it_keeps() {
        use tiff::encoder::colortype::GrayI32;
        // Read at every second pixel, (0, 0), (2, 0), (0, 2) and (2, 2) are kept: only one of
        // them is past 16,777,216, though four of the file's pixels are.
        let big = 50_000_001;
        #[rustfmt::skip]
        let px = [big, big, 0, 0,
                  big, 0, 0, 0,
                  0, 0, 0, 0,
                  0, 0, 0, big];
        let r = read_limited(reader(std::io::Cursor::new(typed::<GrayI32>(4, 4, &px, None))).unwrap(), "ids", 4, None).unwrap();
        assert_eq!((r.width, r.overview), (2, Some((4, 4))));
        assert_eq!(r.rounded, vec![1]);
    }

    #[test]
    fn chunks_decoded_on_several_threads_count_rounded_values() {
        use tiff::encoder::colortype::GrayI32;
        // 64 one-row strips (enough for several threads), read at every eighth pixel: the 64 kept
        // pixels, all past 16,777,216, are counted once each, as one thread counts them.
        let bytes = typed::<GrayI32>(64, 64, &[50_000_001; 64 * 64], None);
        let many = read_threaded(&bytes, 100).unwrap();
        let one = read_limited(reader(std::io::Cursor::new(&bytes)).unwrap(), "one", 100, None).unwrap();
        assert_eq!((many.width, many.overview), (8, Some((64, 64))));
        assert_eq!((many.rounded.as_slice(), one.rounded.as_slice()), (&[64u64][..], &[64u64][..]));
    }

    #[test]
    fn no_data_samples_are_left_out_of_the_count_by_one_rule() {
        use tiff::encoder::colortype::Gray64Float;
        for c in crate::raster::NODATA_CASES {
            let tif = typed::<Gray64Float>(2, 2, &c.samples, c.file);
            let r = read_with_nodata(std::io::Cursor::new(tif), "c", c.sidecar).unwrap();
            assert_eq!(r.rounded.iter().sum::<u64>(), c.counted, "file {:?}, sidecar {:?}", c.file, c.sidecar);
        }
    }

    #[test]
    fn whole_no_data_values_are_compared_exactly() {
        use tiff::encoder::colortype::GrayI64;
        for (file, samples, counted, _) in crate::raster::NODATA_WHOLE_CASES {
            let tif = typed::<GrayI64>(2, 2, samples, Some(file));
            assert_eq!(read(std::io::Cursor::new(tif), "c").unwrap().rounded.iter().sum::<u64>(), *counted, "no-data {file}");
        }
    }
}
