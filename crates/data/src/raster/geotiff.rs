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
use super::{ColorHint, Crs, Exact, GeoTransform, ImageGrid, Raster, Uncounted, max_samples};
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

/// Reads the first image's original dimensions, CRS and full affine grid without decoding
/// pixels, choosing an overview or resampling north-up. Accepts a byte cursor or a seekable
/// file reader. Unlike [`read`], this requires embedded georeferencing and a recognised CRS;
/// it never substitutes a scan's stand-in grid or assumes WGS 84 from coordinate ranges.
///
/// PixelIsPoint tie points are normalised to pixel corners, as in [`ImageGrid`]. Sidecars,
/// subdatasets and externally supplied georeferencing are not applied. This performs synchronous
/// header I/O, with individual tag values limited to 1 MiB: desktop callers should use a
/// background read. Cancel is checked before/after
/// parsing and between tie points; an individual TIFF decoder/header read cannot be interrupted.
///
/// ```no_run
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use rustgis_data::raster::geotiff;
/// let input = std::io::BufReader::new(std::fs::File::open("image.tif")?);
/// let grid = geotiff::read_image_grid(input)?;
/// let native = grid.to_world(0.5, 0.5).ok_or("pixel centre overflows")?;
/// let lonlat = grid.crs().transformer()?.to_lonlat(native).ok_or("outside projection")?;
/// // Keep `grid` for editing in the original pixels, independently of the displayed raster.
/// # let _ = lonlat;
/// # Ok(())
/// # }
/// ```
pub fn read_image_grid<R: Read + Seek>(input: R) -> Result<ImageGrid> {
    crate::progress::check()?;
    let result = (|| {
        let mut limits = Limits::default();
        limits.ifd_value_size = 1 << 20;
        let mut dec = Decoder::new(input).map_err(err)?.with_limits(limits);
        if dec.find_tag_unsigned::<u32>(Tag::NewSubfileType).map_err(err)?.unwrap_or(0) & 5 != 0 {
            return Err(err("image grid requires a full-resolution first image, not an overview or mask"));
        }
        let (width, height) = dec.dimensions().map_err(err)?;
        let keys = image_grid_tags(&mut dec)?;
        let geo = geo_tags_with_keys(&mut dec, &keys)?;
        let crs = geo.crs.ok_or_else(|| match geo.unknown_epsg {
            Some(code) => err(super::crs::unusable_epsg(code)),
            None => err(geo.custom_error.as_deref().unwrap_or("image grid requires a recognised embedded coordinate system")),
        })?;
        let t = geo.transform;
        ImageGrid::new(width, height, crs, geo.affine.unwrap_or([t.origin_x, t.pixel_width, 0., t.origin_y, 0., t.pixel_height]))
    })();
    crate::progress::check()?;
    result
}

/// The exact-grid API refuses malformed georeferencing rather than taking a partial matrix,
/// falling back to another tag, or interpreting inconsistent ground-control points as affine.
fn image_grid_tags<R: Read + Seek>(dec: &mut Decoder<R>) -> Result<Vec<u16>> {
    let mut keys = dec
        .find_tag(Tag::GeoKeyDirectoryTag)
        .map_err(err)?
        .ok_or_else(|| err("image grid requires embedded GeoTIFF keys"))?
        .into_u16_vec()
        .map_err(err)?;
    let Some(&[version, revision, minor, count]) = keys.first_chunk::<4>() else {
        return Err(err("image grid has an incomplete GeoKey directory"));
    };
    if version != 1 || revision != 1 || minor > 1 {
        return Err(err("image grid has an unsupported GeoKey directory version"));
    }
    let entries = keys.get(4..4 + usize::from(count) * 4).ok_or_else(|| err("image grid has an incomplete GeoKey directory"))?;
    let doubles = |dec: &mut Decoder<R>, tag| -> Result<Option<Vec<f64>>> {
        dec.find_tag(Tag::Unknown(tag)).map_err(err)?.map(|v| v.into_f64_vec().map_err(err)).transpose()
    };
    let parameters = doubles(dec, 34736)?.unwrap_or_default();
    let ascii = dec.find_tag(Tag::Unknown(34737)).map_err(err)?.map(|v| v.into_string().map_err(err)).transpose()?.unwrap_or_default();
    for &[key, location, count, value] in entries.as_chunks::<4>().0 {
        crate::progress::check()?;
        let expected_location = match key {
            1026 | 2049 | 3073 | 4097 => Some(34737),
            2053 | 2055 | 2057..=2059 | 2061 | 3077..=3096 => Some(34736),
            1024 | 1025 | 2048 | 2050..=2052 | 2054 | 2056 | 2060 | 3072 | 3074..=3076 | 4096 | 4098 | 4099 => Some(0),
            _ => None,
        };
        let start = usize::from(value);
        let readable = match location {
            0 => count == 1,
            34736 => count == 1 && parameters.get(start).is_some_and(|v| v.is_finite()),
            34737 => count > 0 && ascii.get(start..start + usize::from(count)).is_some(),
            // The CRS reader cannot resolve other storage, including SHORT parameter arrays.
            _ => false,
        };
        if !readable || expected_location.is_some_and(|expected| location != expected) {
            return Err(err(format!("image grid GeoKey {key} has an unreadable parameter reference")));
        }
        if matches!(key, 2048 | 3072) && (location != 0 || count != 1) {
            return Err(err("image grid has an invalid coordinate system GeoKey"));
        }
        if key == 1024 && (location != 0 || count != 1 || !matches!(value, 1 | 2)) {
            return Err(err("image grid requires a projected or geographic coordinate system (GTModelTypeGeoKey 1 or 2)"));
        }
        if key == 1025 && (location != 0 || count != 1 || !matches!(value, 1 | 2)) {
            return Err(err("image grid requires two-dimensional area or point GeoTIFF georeferencing"));
        }
    }
    if let Some(m) = doubles(dec, 34264)? {
        if m.len() != 16 || !m.iter().all(|v| v.is_finite()) || m.get(12..) != Some(&[0., 0., 0., 1.]) {
            return Err(err("image grid requires a finite 4 × 4 affine transformation matrix"));
        }
    } else {
        let scale = doubles(dec, 33550)?.ok_or_else(|| err("image grid requires embedded GeoTIFF georeferencing"))?;
        let ties = doubles(dec, 33922)?.ok_or_else(|| err("image grid requires a pixel scale and tie point"))?;
        if scale.len() != 3 || ties.is_empty() || !ties.len().is_multiple_of(6) || !scale.iter().chain(&ties).all(|v| v.is_finite()) {
            return Err(err("image grid has an invalid pixel scale or tie point"));
        }
        let Some(&[col, row, _, x, y, _]) = ties.as_chunks::<6>().0.first() else {
            return Err(err("image grid has no tie point"));
        };
        for &[c, r, _, wx, wy, _] in ties.as_chunks::<6>().0 {
            crate::progress::check()?;
            let expected = [x + (c - col) * scale[0], y - (r - row) * scale[1]];
            if expected
                .into_iter()
                .zip([wx, wy])
                .any(|(a, b)| !a.is_finite() || (a - b).abs() > 64. * f64::EPSILON * a.abs().max(b.abs()).max(1.))
            {
                return Err(err("image grid tie points do not describe one affine grid"));
            }
        }
    }
    keys.truncate(4 + usize::from(count) * 4);
    Ok(keys)
}

/// Whether the first sample past the colour ones is declared alpha (extra sample 1, associated, or
/// 2, unassociated).
pub(super) fn extra_alpha<R: Read + Seek>(dec: &mut Decoder<R>) -> bool {
    dec.find_tag_unsigned_vec::<u16>(Tag::ExtraSamples).ok().flatten().is_some_and(|v| matches!(v.first(), Some(1 | 2)))
}

/// How `n` samples of a grey image the decoder calls multiband are shown: grey with alpha when
/// the second is declared alpha (extra sample 1, associated, or 2, unassociated), whatever data
/// bands follow it, else grey. GDAL marks the second band of an ordinary two-band file as 0,
/// unspecified data.
pub(super) fn multiband_color<R: Read + Seek>(dec: &mut Decoder<R>, n: u16) -> ColorHint {
    if n >= 2 && extra_alpha(dec) { ColorHint::GrayAlpha } else { ColorHint::Gray }
}

fn geo_tags<R: Read + Seek>(dec: &mut Decoder<R>) -> Result<Geo> {
    let keys: Vec<u16> = dec.find_tag(Tag::GeoKeyDirectoryTag).ok().flatten().and_then(|v| v.into_u16_vec().ok()).unwrap_or_default();
    geo_tags_with_keys(dec, &keys)
}

/// Ordinary readers pass their legacy whole directory; exact grids pass validated declared keys.
fn geo_tags_with_keys<R: Read + Seek>(dec: &mut Decoder<R>, keys: &[u16]) -> Result<Geo> {
    let doubles =
        |dec: &mut Decoder<R>, tag: u16| -> Option<Vec<f64>> { dec.find_tag(Tag::Unknown(tag)).ok().flatten()?.into_f64_vec().ok() };
    let key = |k: u16| -> Option<u16> {
        key_entries(keys)?.iter().find(|[key, location, _, _]| *key == k && *location == 0).map(|[_, _, _, value]| *value)
    };
    let pixel_is_point = key(1025) == Some(2);
    let geographic_model = key(1024) == Some(2);
    let usable = |c: &u16| *c != 32767 && *c != 0;
    let gdoubles = doubles(dec, 34736).unwrap_or_default();
    let gascii = dec.find_tag(Tag::Unknown(34737)).ok().flatten().and_then(|v| v.into_string().ok()).unwrap_or_default();
    // A projected file with a user-defined projection (the NLCD's Albers, say) defines it in its
    // keys: read as its own system, never as the geographic system it's based on.
    // So is one in an ESRI PE citation, which GDAL writes with a user-defined model type (32767).
    let user_defined = (key(1024) == Some(1) && key(3072).is_none_or(|c| !usable(&c))) || key(1024) == Some(32767);
    let citation = |k: u16| match geokey(keys, &gdoubles, &gascii, k) {
        Some(KeyValue::Ascii(t)) if !t.trim().is_empty() => Some(t),
        _ => None,
    };
    // PCSCitation, else GTCitation.
    let system = || citation(3073).or_else(|| citation(1026)).unwrap_or_else(|| "the file's own projection".into());
    // A projection of another body's planetographic longitude/latitude is refused when a citation
    // says so, as GDAL names one. GeoTIFF has no key for axis directions, and PROJ gives such a
    // projection westings.
    if user_defined
        && planetary_axes(keys, &gdoubles, &gascii).is_some()
        // PCSCitation, GTCitation, GeogCitation.
        && [3073, 1026, 2049].into_iter().filter_map(citation).any(|t| super::planetary::ographic_name(&t))
    {
        return Err(err(super::planetary::refused(&system(), super::planetary::OGRAPHIC_PROJECTED)));
    }
    // A projection the keys don't describe well enough is left unknown (the open goes on, and fails
    // later only if the coordinates aren't degrees), its reason kept for the message.
    let mut custom_error = None;
    let by_code = || {
        let code = key(3072).filter(usable).or(key(2048).filter(usable)).map(u32::from);
        (code, code.and_then(Crs::from_epsg))
    };
    let (code, crs) = if user_defined {
        match custom_crs(keys, &gdoubles, &gascii) {
            Ok(crs) => (None, Some(crs)),
            // A user-defined model type whose keys can't be read, but that names a code, is read
            // by the code, as before.
            Err(_) if key(1024) == Some(32767) && by_code().1.is_some() => by_code(),
            Err(e) => {
                custom_error = Some(e.to_string());
                (None, None)
            }
        }
    } else {
        by_code()
    };
    // Another body's system ([`super::planetary`]). A geographic one is its own system, not WGS 84:
    // its longitude/latitude are read as they are, and a GeoTIFF written from it keeps its keys.
    // A projection of it that can't be read is refused, because its metres would be read as
    // degrees.
    let crs = match crs {
        None if geographic_model && key(2048).is_none_or(|c| !usable(&c)) => planetary_geographic(keys, &gdoubles, &gascii)?.or(crs),
        crs => crs,
    };
    if let Some(e) = &custom_error
        && planetary_axes(keys, &gdoubles, &gascii).is_some()
    {
        return Err(err(super::planetary::refused(&system(), e.strip_prefix("GeoTIFF: ").unwrap_or(e))));
    }

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
pub(crate) fn cog_geo<R: Read + Seek>(dec: &mut Decoder<R>) -> Result<Option<(Crs, GeoTransform, Option<Exact>, bool)>> {
    let geo = geo_tags(dec)?;
    if geo.affine.is_some() || !geo.referenced {
        return Ok(None);
    }
    let assumed = geo.crs.is_none() && geo.geographic_model;
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
    Ok(Some((crs, geo.transform, geo.nodata_exact, assumed)))
}

/// What reading a geographic GeoTIFF without an EPSG code assumes, said whether it is read whole or
/// in place.
pub(crate) const NO_EPSG_GEOGRAPHIC: &str =
    "its geographic coordinate system has no EPSG code, so its datum isn't read; its coordinates are read as longitude/latitude on WGS 84";

/// The file's Copyright tag, trimmed, or why it is left out (`Err`); `Ok(None)` without one.
pub(crate) fn copyright_tag<R: Read + Seek>(dec: &mut Decoder<R>) -> std::result::Result<Option<String>, String> {
    match dec.find_tag(Tag::Copyright).map(|v| v.map(|v| v.into_string())) {
        Ok(None) => Ok(None),
        Ok(Some(Ok(c))) => Ok(Some(c.trim().to_string())),
        Ok(Some(Err(e))) => Err(format!("its Copyright tag: it isn't ASCII text ({e}), so it is left out")),
        Err(e) => Err(format!("its Copyright tag: it can't be read ({e}), so it is left out")),
    }
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

/// Preserve ordinary readers' legacy scan when a file misstates its key count. The exact-grid
/// API passes only the declared entries, validated separately, so it cannot scan parameter tails.
fn key_entries(keys: &[u16]) -> Option<&[[u16; 4]]> {
    Some(keys.get(4..)?.as_chunks::<4>().0)
}

fn geokey(keys: &[u16], doubles: &[f64], ascii: &str, k: u16) -> Option<KeyValue> {
    let [_, location, count, value] = *key_entries(keys)?.iter().find(|e| e[0] == k)?;
    match location {
        0 => Some(KeyValue::Short(value)),
        34736 => doubles.get(value as usize).copied().map(KeyValue::Double),
        34737 => ascii
            .get(value as usize..(value as usize).saturating_add(count as usize))
            .map(|s| KeyValue::Ascii(s.trim_end_matches(['|', '\0']).to_string())),
        _ => None,
    }
}

/// Metres per unit of the ellipsoid's axes in a file's keys (GeogLinearUnits, metres when it isn't
/// given). `a` is the semi-major axis as the keys give it. When that is already the Earth's in
/// metres, the factor is 1, because some writers give metres whatever unit they declare. `None`
/// for a unit the keys don't size.
fn axis_unit(keys: &[u16], doubles: &[f64], ascii: &str, a: f64) -> Option<f64> {
    if !super::planetary::off_earth(a) {
        return Some(1.0);
    }
    // GeogLinearUnits
    match geokey(keys, doubles, ascii, 2052) {
        None | Some(KeyValue::Short(9001)) => Some(1.0),      // metre
        Some(KeyValue::Short(9002)) => Some(0.3048),          // foot
        Some(KeyValue::Short(9003)) => Some(1200.0 / 3937.0), // US survey foot
        Some(KeyValue::Short(9036)) => Some(1000.0),          // kilometre
        // GeogLinearUnitSize
        _ => match geokey(keys, doubles, ascii, 2053) {
            Some(KeyValue::Double(f)) if f.is_finite() && f > 0.0 => Some(f),
            _ => None,
        },
    }
}

/// The keys and double parameters with the ellipsoid's axes in metres ([`axis_unit`]), as GDAL
/// 3.10 doesn't read them: it takes them for metres whatever GeogLinearUnits says, so the Earth's
/// semi-major axis in feet would be another body's. Axes in a unit the keys don't size are an error.
fn axes_in_metres(keys: &[u16], doubles: &[f64], ascii: &str) -> Result<(Vec<u16>, Vec<f64>)> {
    // The axes are read from the file's values, not from the slots added below.
    let original = doubles;
    let (mut keys, mut doubles) = (keys.to_vec(), doubles.to_vec());
    let Some(KeyValue::Double(a)) = geokey(&keys, &doubles, ascii, 2057) else { return Ok((keys, doubles)) };
    let to_m = axis_unit(&keys, &doubles, ascii, a).ok_or_else(|| {
        let unit = match geokey(&keys, &doubles, ascii, 2052) {
            Some(KeyValue::Short(u)) => format!(" (GeogLinearUnits {u})"),
            _ => String::new(),
        };
        err(format!("the file's ellipsoid axes are in a unit{unit} that isn't supported"))
    })?;
    if to_m != 1.0 {
        // GeogSemiMajorAxis and GeogSemiMinorAxis, each in metres in a value of its own after the
        // others (keys may share one value, which would be converted twice, or another key's),
        // then GeogLinearUnits as metres.
        for entry in keys.get_mut(4..).into_iter().flat_map(|k| k.as_chunks_mut::<4>().0) {
            match entry {
                [2057 | 2058, 34736, _, offset] => {
                    // An axis whose value is missing would stay in its unit, read as metres.
                    let metres = original
                        .get(usize::from(*offset))
                        .map(|v| v * to_m)
                        .ok_or_else(|| err("the file's ellipsoid axis points past its double parameters"))?;
                    // A slot past the last a key can address: the axes can't be put in metres.
                    let at = u16::try_from(doubles.len())
                        .map_err(|_| err("the file's ellipsoid axes can't be converted to metres: too many double parameters"))?;
                    doubles.push(metres);
                    *offset = at;
                }
                [2052, 0, _, value] => *value = 9001,
                _ => {}
            }
        }
    }
    Ok((keys, doubles))
}

/// The semi-major and semi-minor axes, in metres, of the ellipsoid a file's keys give by its axes
/// (GeogSemiMajorAxis with GeogSemiMinorAxis or GeogInvFlattening, in GeogLinearUnits), when it is
/// another body's than the Earth's ([`super::planetary`]).
fn planetary_axes(keys: &[u16], doubles: &[f64], ascii: &str) -> Option<(f64, f64)> {
    let num = |k| match geokey(keys, doubles, ascii, k) {
        Some(KeyValue::Double(v)) if v.is_finite() && v > 0.0 => Some(v),
        _ => None,
    };
    let raw = num(2057)?; // GeogSemiMajorAxis
    let to_m = axis_unit(keys, doubles, ascii, raw)?;
    let a = raw * to_m;
    // GeogSemiMinorAxis, GeogInvFlattening
    let b = match (num(2058), num(2059)) {
        (Some(b), _) => b * to_m,
        (None, Some(rf)) => a - a / rf,
        (None, None) => a,
    };
    super::planetary::off_earth(a).then_some((a, b))
}

/// A geographic file's own system when it is another body's: its ellipsoid by its axes, named by
/// its citation (`GCS Name = Mars (2015) - Sphere / Ocentric|…`), with the keys it came in. `None`
/// for the Earth's. It is an error when its cells would land in the wrong place as
/// longitude/latitude in degrees:
/// - a planetographic system, told by its citation as GDAL names an IAU `…01` system (GeoTIFF has
///   no key for longitudes counted west);
/// - other angular units than degrees.
fn planetary_geographic(keys: &[u16], doubles: &[f64], ascii: &str) -> Result<Option<Crs>> {
    let Some((a, b)) = planetary_axes(keys, doubles, ascii) else { return Ok(None) };
    // GeogCitation
    let citation = match geokey(keys, doubles, ascii, 2049) {
        Some(KeyValue::Ascii(t)) => t,
        _ => String::new(),
    };
    let first = citation.split('|').next().unwrap_or("");
    let label = match first.trim().strip_prefix("GCS Name =").unwrap_or(first).trim() {
        "" => format!("Geographic coordinate system (semi-major axis {a} m)"),
        name => name.to_string(),
    };
    if super::planetary::ographic_name(first) {
        return Err(err(super::planetary::refused(&label, super::planetary::OGRAPHIC)));
    }
    let degree = |f: f64| (f - 1f64.to_radians()).abs() < 1e-12;
    // GeogAngularUnits
    let unit = match geokey(keys, doubles, ascii, 2054) {
        // Degrees, the default (9102); 9122 is the degree "supplier to define representation".
        None | Some(KeyValue::Short(9102 | 9122)) => None,
        // User-defined, sized by GeogAngularUnitSize in radians.
        Some(KeyValue::Short(32767)) => match geokey(keys, doubles, ascii, 2055) {
            Some(KeyValue::Double(f)) if degree(f) => None,
            Some(KeyValue::Double(f)) => Some(format!("units of {f} radians")),
            _ => Some("a user-defined unit of no size".to_string()),
        },
        Some(KeyValue::Short(code)) => Some(format!("GeogAngularUnits {code}")),
        Some(_) => Some("an unreadable GeogAngularUnits".to_string()),
    };
    if let Some(unit) = unit {
        let why = format!("its longitude/latitude are in {unit}, not degrees");
        return Err(err(super::planetary::refused(&label, &why)));
    }
    // A prime meridian other than the body's reference one, in degrees (GeogPrimeMeridianLong).
    let pm = match geokey(keys, doubles, ascii, 2061) {
        Some(KeyValue::Double(pm)) if pm.is_finite() && pm != 0.0 => format!(" +pm={pm}"),
        _ => String::new(),
    };
    let def = CustomCrs {
        proj4: format!("+proj=longlat +a={a} +b={b}{pm} +no_defs"),
        label,
        geokeys: keys.to_vec(),
        doubles: doubles.to_vec(),
        ascii: ascii.to_string(),
        wkt: None,
    };
    Crs::custom(def).map(Some).map_err(err)
}

/// A user-defined projection from a file's GeoKeys, as terrio-crs reads them (GDAL's
/// `GTIFGetOGISDefnAsOSR`, ESRI PE citations included), kept by its WKT. One the keys say is an
/// EPSG system RustGIS knows (a UTM zone by its citation) is that system.
fn custom_crs(keys: &[u16], doubles: &[f64], ascii: &str) -> Result<Crs> {
    let (in_metres, metres) = axes_in_metres(keys, doubles, ascii)?;
    let read = terrio_crs::geokeys::read(&in_metres, &metres, ascii.as_bytes());
    let sr = match (read.crs, read.unsupported_method) {
        (Some(sr), _) => sr,
        (None, Some(c)) => return Err(err(format!("the file's own projection (coordinate transformation {c}) isn't supported"))),
        (None, None) => return Err(err("the file's own projection isn't described in its keys")),
    };
    // Not when the keys give a shift of their own (an ESRI PE citation's TOWGS84): that would be
    // set aside for the code's, as a WKT with its own shift isn't read as the code beside it.
    let own_shift = sr.to_wkt().is_some_and(|w| shift_differs(&w, sr.epsg));
    if let Some(crs) = sr.epsg.filter(|_| !own_shift).and_then(Crs::from_epsg) {
        return Ok(crs);
    }
    let text = |k| match geokey(keys, doubles, ascii, k) {
        Some(KeyValue::Ascii(s)) if !s.trim().is_empty() => Some(s),
        _ => None,
    };
    let label = text(3073).or_else(|| text(1026)).or_else(|| sr.name()).unwrap_or_else(|| "Custom projection".into());
    let wkt = sr.to_wkt().ok_or_else(|| err("the file's own projection has no definition"))?;
    // Where PROJ writes no string, the one RustGIS wrote before, as for WKT, so a project saved
    // with the system still opens in an older release.
    let proj4 = sr.to_proj4().map(|d| d.replace(" +type=crs", "")).or_else(|| super::wkt::legacy_proj4(&wkt)).unwrap_or_default();
    Crs::custom(CustomCrs { proj4, label, geokeys: keys.to_vec(), doubles: doubles.to_vec(), ascii: ascii.to_string(), wkt: Some(wkt) })
        .map_err(err)
}

/// Whether WKT `wkt` gives a shift of its own (a `TOWGS84`, a `BOUNDCRS`) that places points
/// otherwise than the EPSG `code` it is identified as ([`super::wkt::places_as`]); a shift equal to
/// the code's is the code.
fn shift_differs(wkt: &str, code: Option<u32>) -> bool {
    let upper: String = wkt.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_uppercase();
    (upper.contains("TOWGS84[") || upper.contains("BOUNDCRS["))
        && code.and_then(Crs::from_epsg).is_none_or(|c| !super::wkt::places_as(wkt, c))
}

/// Whether a raster in `crs` can be written as a GeoTIFF: an EPSG system always, a user-defined
/// one when it came with GeoKeys or can be written as them (see [`terrio_keys`]).
pub fn describes(crs: Crs) -> bool {
    crs.custom_def().is_none_or(|d| !d.geokeys.is_empty() || terrio_keys(&d).is_some())
}

/// GeoKeys for a user-defined system as GDAL 3.12.1 writes them (terrio-crs's
/// `SpatialRef::to_geokeys`): from its WKT, else its PROJ definition; a system the keys can't
/// hold goes in an ESRI PE citation. The key directory (with the raster type as area), its
/// double parameters and its ASCII parameters; `None` when it can't be written at all.
fn terrio_keys(d: &CustomCrs) -> Option<(Vec<u16>, Vec<f64>, String)> {
    // A system with a shift of its own keeps it: terrio-crs writes its own keys or its declared
    // code with GeogTOWGS84GeoKey (2062), never a code it is only identified as.
    let sr = match &d.wkt {
        Some(w) => terrio_crs::SpatialRef::from_wkt(w),
        None => terrio_crs::SpatialRef::from_proj4(&format!("{} +type=crs", d.proj4)).ok()?,
    };
    let tags = sr.to_geokeys().ok()?.encode().ok()?;
    Some((with_raster_type(&tags.directory)?, tags.doubles, tags.ascii))
}

/// Key directory `keys` with GTRasterTypeGeoKey (1025) as area (1), added in its place by id;
/// `None` when the directory can't count its keys.
fn with_raster_type(keys: &[u16]) -> Option<Vec<u16>> {
    let (Some(head), Some(entries)) = (keys.get(..4), keys.get(4..)) else { return Some(keys.to_vec()) };
    let mut entries: Vec<[u16; 4]> = entries.as_chunks::<4>().0.iter().copied().filter(|e| e[0] != 1025).collect();
    entries.push([1025, 0, 1, 1]);
    entries.sort_by_key(|e| e[0]);
    let mut out = vec![head[0], head[1], head[2], u16::try_from(entries.len()).ok()?];
    out.extend(entries.iter().flatten());
    Some(out)
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

/// Decodes chunk `index` (a strip or tile) of the current image: its samples, and which of them
/// its own encoding marks invalid (LERC's validity masks), for the caller to make no-data with
/// [`super::lerc::blank`]. The `tiff` crate fails on some LZW chunks it should read (issue #350):
/// those are decoded by [`super::lzw`] instead. It has no LERC codec: LERC chunks are decoded by
/// [`super::lerc`] (#1818), `table` keeping the image's chunk offsets from one call to the next.
pub(crate) fn read_chunk<R: Read + Seek>(
    dec: &mut Decoder<R>,
    index: u32,
    table: &mut super::lerc::Table,
) -> Result<(DecodingResult, Option<Vec<bool>>)> {
    if super::lerc::is_lerc(dec) {
        let c = super::lerc::chunk(dec, index, table)?;
        return Ok((c.samples, c.gaps));
    }
    match dec.read_chunk(index) {
        Ok(v) => Ok((v, None)),
        Err(e) if lzw(dec) => super::lzw::chunk(dec, index).map(|v| (v, None)).map_err(|_| err(e)),
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

/// A reader that shows palette images, and RGB images with samples past the colour ones, as grey
/// ones: the `tiff` crate decodes no palette images, but decodes their colour indices as grey
/// samples, and the colour table is read from the ColorMap tag as it is. An RGB image with an
/// unspecified fourth sample (RGBI) decoded as RGB loses that sample before the predictor is
/// undone, corrupting the colours (https://github.com/image-rs/image-tiff/issues/425); as grey,
/// every sample is decoded. The only change is the PhotometricInterpretation value (→ 1) in each
/// such image's IFD, in memory.
///
/// It also shows a SampleFormat of 4 ("undefined", which CBERS-4 scenes have) as 1, unsigned, as
/// libtiff and GDAL read it: the `tiff` crate refuses to decode it (#2593).
pub(crate) struct Unpaletted<R> {
    inner: R,
    /// Where each patched PhotometricInterpretation or SampleFormat value is, and what to show there.
    patches: Vec<(u64, [u8; 2])>,
    /// Whether the first image (the full one) is a palette image.
    first_palette: bool,
    /// Whether the first image is an RGB image with samples past the colour ones (shown to the
    /// decoder as grey): its first three samples are red, green and blue, every sample read.
    first_wide_rgb: bool,
    pos: u64,
}

/// Most images and IFD entries looked at for palette images, and SampleFormat values in an image.
const MAX_IFDS: usize = 64;
const MAX_ENTRIES: u64 = 4096;

/// Where the palette images' PhotometricInterpretation values are, and those of RGB images with
/// samples past the colour ones (which the decoder would leave out, or refuse), walking the IFD
/// chain (classic and BigTIFF). A file it can't walk has none: the decoder reports what's wrong.
/// Each SampleFormat value of 4 (undefined) is patched to 1 too, in the entry or in the array it
/// points to (#2593). Returns the patches, whether the first image is a palette image, and whether
/// it is such an RGB image.
fn palette_patches<R: Read + Seek>(r: &mut R) -> (Vec<(u64, [u8; 2])>, bool, bool) {
    use std::io::SeekFrom;
    let mut patches = Vec::new();
    // Whether the first image is one of them, and whether it is an RGB image with more samples.
    let (mut first, mut first_wide) = (false, false);
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
            let value_at = if big { 12 } else { 8 };
            // The SHORT 1: grey as a PhotometricInterpretation, unsigned as a SampleFormat.
            let one = if little { [1, 0] } else { [0, 1] };
            // PhotometricInterpretation (262): where its value is, and the value; SamplesPerPixel
            // (277); the first ExtraSamples (338) value, when it is inline; SampleFormat (339):
            // where its values are (in the entry when they fit there, else where it points) and
            // how many. Each a SHORT, as the decoder itself requires of them.
            let (mut photometric, mut samples, mut extra, mut format) = (None, 1u16, None, None);
            for (i, e) in entries.chunks(entry_len as usize).enumerate() {
                let short = u16_(e.get(2..)?)? == 3;
                let entry_value = ifd + count_len + i as u64 * entry_len + value_at as u64;
                match u16_(e)? {
                    262 if short => photometric = Some((entry_value, u16_(e.get(value_at..)?)?)),
                    277 if short => samples = u16_(e.get(value_at..)?)?,
                    338 if short => extra = u16_(e.get(value_at..)?),
                    339 if short => {
                        let count = if big { u64_(e.get(4..)?)? } else { u64::from(u32_(e.get(4..)?)?) };
                        // Two bytes a value: four fit in a BigTIFF entry, two in a classic one.
                        let at = if count <= if big { 4 } else { 2 } {
                            entry_value
                        } else if big {
                            u64_(e.get(value_at..)?)?
                        } else {
                            u64::from(u32_(e.get(value_at..)?)?)
                        };
                        format = (count <= MAX_ENTRIES).then_some((at, count));
                    }
                    _ => {}
                }
            }
            match photometric {
                Some((at, 3)) => {
                    patches.push((at, one));
                    first |= k == 0;
                }
                // An RGB image whose fourth sample isn't alpha, or with five or more: RGBI
                // orthophotos, multispectral images.
                Some((at, 2)) if samples > 4 || (samples == 4 && !matches!(extra, Some(1 | 2))) => {
                    patches.push((at, one));
                    first_wide |= k == 0;
                }
                _ => {}
            }
            let mut next = [0u8; 8];
            r.read_exact(&mut next[..next_len]).ok()?;
            ifd = if big { u64_(&next)? } else { u64::from(u32_(&next)?) };
            // Each SampleFormat value of 4 (undefined) is shown as 1, unsigned, as libtiff and GDAL
            // read it (#2593). Values that can't be read are left to the decoder.
            if let Some((at, count)) = format {
                let mut values = vec![0u8; count as usize * 2];
                if r.seek(SeekFrom::Start(at)).is_ok() && r.read_exact(&mut values).is_ok() {
                    for (j, v) in values.as_chunks::<2>().0.iter().enumerate() {
                        if u16_(v.as_slice()) == Some(4) {
                            patches.push((at + 2 * j as u64, one));
                        }
                    }
                }
            }
        }
        Some(())
    };
    let _ = walk();
    (patches, first, first_wide)
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

    /// Whether the first image is an RGB image with more samples than its colours.
    pub(crate) fn first_is_wide_rgb(&self) -> bool {
        self.first_wide_rgb
    }
}

impl<R: Read + Seek> Unpaletted<R> {
    fn new(mut inner: R) -> std::io::Result<Self> {
        let (patches, first_palette, first_wide_rgb) = palette_patches(&mut inner);
        inner.seek(std::io::SeekFrom::Start(0))?;
        Ok(Unpaletted { inner, patches, first_palette, first_wide_rgb, pos: 0 })
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

/// The columns and rows (half-open) of a grid read that are kept: of every `k`-th pixel of the
/// image each way, as [`decimate`] reads it, all of them for a whole image, else those of a part
/// ([`super::Part`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Span {
    pub(super) cols: (u32, u32),
    pub(super) rows: (u32, u32),
}

impl Span {
    /// The whole of a grid `w` × `h`.
    pub(super) fn whole(w: u32, h: u32) -> Span {
        Span { cols: (0, w), rows: (0, h) }
    }

    pub(super) fn width(self) -> usize {
        self.cols.1.saturating_sub(self.cols.0) as usize
    }

    pub(super) fn height(self) -> usize {
        self.rows.1.saturating_sub(self.rows.0) as usize
    }

    /// Its columns and rows.
    pub(super) fn size(self) -> (u32, u32) {
        (self.cols.1.saturating_sub(self.cols.0), self.rows.1.saturating_sub(self.rows.0))
    }

    fn is_empty(self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    /// The image's pixels (half-open, every `k`-th one) within `len` of them from `at` that the
    /// span's `range` (of the grid read) keeps: the first is a multiple of `k`.
    pub(super) fn kept(self, range: (u32, u32), at: u64, len: u64, k: u64) -> (u64, u64) {
        let start = at.max(u64::from(range.0) * k).div_ceil(k) * k;
        (start, (at + len).min(u64::from(range.1) * k))
    }
}

/// The columns and rows of a chunk (a tile, or a strip of the image's width) of the full image of
/// the GeoTIFF at `path`, as its tags give them; `None` when it can't be read.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn chunk_size(path: &std::path::Path) -> Option<(u32, u32)> {
    let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let mut dec = Decoder::new(reader(file).ok()?).ok()?.with_limits(Limits::default());
    let (w, h) = dec.dimensions().ok()?;
    let c = Chunks::new(&mut dec, w, h, 1, false);
    Some((c.cw, c.ch))
}

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

    /// The most one chunk holds while it is decoded: its samples as read (up to 8 bytes each),
    /// as 32-bit floats, and which of them were rounded (a byte each).
    #[cfg(not(target_arch = "wasm32"))]
    fn bytes(&self) -> usize {
        (self.cw as usize).saturating_mul(self.ch as usize).saturating_mul(self.samples).saturating_mul(13)
    }

    #[cfg(test)]
    fn count(&self) -> u64 {
        u64::from(self.per_plane) * self.planes as u64
    }

    /// The bytes the first chunk decodes to at `bits` a sample, each row padded to whole bytes: as
    /// the tiff crate holds it, cropped to the image, then with a tile's padding, as the LZW and
    /// LERC readers hold it. The first chunk is the largest (only the last strip and the edge tiles are
    /// smaller), and one always read, since it holds the image's first pixel.
    fn first_bytes(&self, bits: u16) -> (u64, u64) {
        let size = |cols: u32, rows: u32| {
            let row_bits = u64::from(cols).saturating_mul(self.samples as u64).saturating_mul(u64::from(bits));
            row_bits.div_ceil(8).saturating_mul(u64::from(rows))
        };
        // The tiff crate gives a bottom tile of every band after the first, when the bands are stored
        // apart, its padding rows too (see `place`); a strip's rows are within the image already.
        let rows = if self.planes > 1 { self.ch } else { self.ch.min(self.h) };
        (size(self.cw.min(self.w), rows), size(self.cw, self.ch))
    }

    /// Where chunk `c` of a plane lies: its corner, then the columns and rows of it the image
    /// holds (the rest is padding). In `u64`: `x0 + cw` and the next multiple of a step pass
    /// `u32::MAX` in an image that wide, which in `u32` panics in a debug build and wraps in a
    /// release build.
    fn span(&self, c: u32) -> (u64, u64, u64, u64) {
        let across = self.across.max(1);
        let (x0, y0) = (u64::from(c % across) * u64::from(self.cw), u64::from(c / across) * u64::from(self.ch));
        let (w, h) = (u64::from(self.w), u64::from(self.h));
        (x0, y0, u64::from(self.cw).min(w.saturating_sub(x0)), u64::from(self.ch).min(h.saturating_sub(y0)))
    }

    /// The chunk columns and rows (half-open) holding a pixel of `span` (of every `k`-th pixel
    /// each way): every chunk for the whole image.
    fn covering(&self, span: Span, k: u32) -> ((u64, u64), (u64, u64)) {
        let k = u64::from(k.max(1));
        let reach = |(a, b): (u32, u32), size: u32, image: u32| {
            // The image's pixels the span reaches, to its end: for the whole image, every chunk
            // (one past its last kept pixel is still looked at, as before).
            let (first, end) = (u64::from(a) * k, (u64::from(b) * k).min(u64::from(image)));
            (first / u64::from(size.max(1)), end.div_ceil(u64::from(size.max(1))))
        };
        let (cols, rows) = (reach(span.cols, self.cw, self.w), reach(span.rows, self.ch, self.h));
        let down = u64::from(self.per_plane / self.across.max(1));
        ((cols.0, cols.1.min(u64::from(self.across))), (rows.0, rows.1.min(down)))
    }

    /// How many chunks [`Chunks::nth`] goes through for `span`, every plane's.
    fn count_in(&self, span: Span, k: u32) -> u64 {
        if span.is_empty() {
            return 0;
        }
        let ((c0, c1), (r0, r1)) = self.covering(span, k);
        c1.saturating_sub(c0).saturating_mul(r1.saturating_sub(r0)).saturating_mul(self.planes as u64)
    }

    /// The `p`-th chunk holding a pixel of `span` (plane by plane, then row by row), counting
    /// through every plane as [`Chunks::wanted`] takes it: for the whole image, `p` itself.
    fn nth(&self, p: u64, span: Span, k: u32) -> Option<u64> {
        let ((c0, c1), (r0, r1)) = self.covering(span, k);
        let (across, down) = (c1.checked_sub(c0).filter(|n| *n > 0)?, r1.checked_sub(r0).filter(|n| *n > 0)?);
        let per_plane = across.checked_mul(down)?;
        let (plane, at) = (p / per_plane, p % per_plane);
        let c = (r0 + at / across).checked_mul(u64::from(self.across))?.checked_add(c0 + at % across)?;
        plane.checked_mul(u64::from(self.per_plane))?.checked_add(c)
    }

    /// Chunk `i` (counting through every plane) as (plane, chunk index in the file), when it holds
    /// a kept row and column of `span`; a chunk holding none is skipped unread.
    fn wanted(&self, i: u64, k: u32, span: Span) -> Option<(usize, u32)> {
        let plane = usize::try_from(i / u64::from(self.per_plane.max(1))).ok()?;
        let c = u32::try_from(i % u64::from(self.per_plane.max(1))).ok()?;
        let (x0, y0, cols, rows) = self.span(c);
        let k = u64::from(k.max(1));
        let (xs, xe) = span.kept(span.cols, x0, cols, k);
        let (ys, ye) = span.kept(span.rows, y0, rows, k);
        if xs >= xe || ys >= ye {
            return None;
        }
        let index = u32::try_from(plane).ok()?.checked_mul(self.per_plane)?.checked_add(c)?;
        Some((plane, index))
    }

    /// Copies every `k`-th pixel each way of chunk `c` of `plane` (decoded as `values`) that
    /// `span` keeps into `bands`, each the span's width, counting in `rounded` (by band) the
    /// pixels copied whose bit is set in `changed` (from [`rounded_bits`]). Values of none of the
    /// chunk's shapes are an error: a row length guessed from them would put pixels in other
    /// pixels' places.
    fn place(
        &self,
        bands: &mut [Vec<f32>],
        values: &[f32],
        changed: Option<&[u64]>,
        rounded: &mut [u64],
        plane: usize,
        c: u32,
        k: u32,
        span: Span,
    ) -> Result<()> {
        let c = c % self.per_plane.max(1);
        let ow = span.width();
        let (x0, y0, cols, rows) = self.span(c);
        let k = u64::from(k.max(1));
        // The chunk's row length: the data width, or the full tile width when padded. The tiff
        // crate gives a bottom tile of every band after the first, when the bands are stored
        // apart, its padding rows too (issue #544), so the row count can't tell it alone. Where two
        // shapes have the same size, the data width wins: both the tiff crate and our LZW decoder
        // return chunks as wide as the data.
        let per_pixel = self.samples.max(1);
        let (dw, data_rows, cw, ch) = (cols as usize, rows as usize, self.cw as usize, self.ch as usize);
        let shapes = [(dw, data_rows), (dw, ch), (cw, data_rows), (cw, ch)];
        let stride = shapes
            .iter()
            .find(|(w, r)| w.saturating_mul(*r).saturating_mul(per_pixel) == values.len())
            .map(|(w, _)| (*w).max(1))
            .ok_or_else(|| {
                err(format!(
                    "a strip or tile decodes to {} values, not the {dw} × {data_rows} pixels of {per_pixel} samples it holds",
                    values.len()
                ))
            })?;
        let ((xs, xe), (ys, ye)) = (span.kept(span.cols, x0, cols, k), span.kept(span.rows, y0, rows, k));
        let (sc, sr) = (u64::from(span.cols.0), u64::from(span.rows.0));
        // Every pixel kept and none to count: a run of a row at a time. Threads reading an image
        // take turns placing their chunks, so this part of a read isn't shared among them.
        if k == 1 && changed.is_none() {
            let (width, left) = (xe.saturating_sub(xs) as usize, (xs - x0) as usize);
            for y in ys..ye {
                let src = ((y - y0) as usize * stride + left) * per_pixel;
                let dst = (y - sr) as usize * ow + (xs - sc) as usize;
                let Some(src) = values.get(src..src + width * per_pixel) else { continue };
                for b in 0..self.samples {
                    let band = if self.planar { plane } else { b };
                    let Some(row) = bands.get_mut(band).and_then(|v| v.get_mut(dst..dst + width)) else { continue };
                    if per_pixel == 1 {
                        row.copy_from_slice(src);
                    } else {
                        for (slot, v) in row.iter_mut().zip(src.iter().skip(b).step_by(per_pixel)) {
                            *slot = *v;
                        }
                    }
                }
            }
            return Ok(());
        }
        let mut y = ys;
        while y < ye {
            let mut x = xs;
            while x < xe {
                // With `stride` one of the chunk's shapes, every position is inside `values`; the
                // checked arithmetic only keeps a product from being computed unchecked.
                let src = usize::try_from(y - y0)
                    .ok()
                    .and_then(|row| row.checked_mul(stride))
                    .and_then(|at| at.checked_add(usize::try_from(x - x0).ok()?))
                    .and_then(|at| at.checked_mul(self.samples));
                let dst = (y / k - sr) as usize * ow + (x / k - sc) as usize;
                for b in 0..self.samples {
                    let band = if self.planar { plane } else { b };
                    let at = src.unwrap_or(usize::MAX).saturating_add(b);
                    if let (Some(v), Some(slot)) = (values.get(at), bands.get_mut(band).and_then(|v| v.get_mut(dst))) {
                        *slot = *v;
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
        Ok(())
    }
}

/// The most bytes the tiff crate decodes one chunk to, or a whole image to: the sample limit as
/// 32-bit floats.
fn decoding_limit() -> usize {
    max_samples().saturating_mul(4)
}

/// The tiff crate's limits for reading a whole file's image: [`decoding_limit`] decoded.
fn limits() -> Limits {
    let mut l = Limits::default();
    l.decoding_buffer_size = decoding_limit();
    l.intermediate_buffer_size = 256 << 20;
    l.ifd_value_size = 64 << 20;
    l
}

/// Fewest chunks worth decoding on more than one thread.
#[cfg(not(target_arch = "wasm32"))]
const PARALLEL_CHUNKS: u64 = 16;

/// Reads an image a strip or tile at a time, keeping every `k`-th pixel each way: an overview
/// made as it is read, for a file too large to hold that has none. Returns the bands, of the
/// pixels `span` keeps (`None`: all of them). With `k` 1, the whole image a chunk at a time.
/// With `open`, chunks are decoded on several threads, each reading image `image` of its own
/// copy of the file. Only the chunks holding a pixel of the span are looked at.
/// Progress counts chunks examined, including ones skipped by the overview. Cancel is checked
/// between chunks; decoding and placing an individual chunk cannot be interrupted.
fn decimate<R: Read + Seek>(
    dec: &mut Decoder<R>,
    w: u32,
    h: u32,
    nbands: usize,
    planar: bool,
    k: u32,
    span: Option<Span>,
    nodata: Uncounted,
    open: Option<(Opener<'_>, usize, Limits)>,
) -> Result<(Vec<Vec<f32>>, &'static str, Vec<u64>)> {
    crate::progress::check()?;
    let k = k.max(1);
    let span = span.unwrap_or_else(|| Span::whole(w.div_ceil(k), h.div_ceil(k)));
    let (ow, oh) = (span.width(), span.height());
    let chunks = Chunks::new(dec, w, h, nbands, planar);
    let examined = chunks.count_in(span, k);
    // A chunk larger than its decoder takes can't be read: said before the bands are set aside,
    // which can be gigabytes for a header claiming a vast image (a few hundred bytes of file, found
    // by fuzzing, #1952). LERC chunks go to its reader only, which holds 8 bytes a sample with a
    // tile's padding; LZW ones to the tiff crate, then to our reader, which keeps the padding too;
    // the rest to the tiff crate only, which holds a chunk cropped to the image.
    let sample_bits = bits(dec);
    let (cropped, padded) = chunks.first_bytes(sample_bits);
    // The LZW reader takes 8- to 64-bit samples only.
    let lzw_reads = lzw(dec) && matches!(sample_bits, 8 | 16 | 32 | 64);
    let refused = if super::lerc::is_lerc(dec) {
        let lerc = chunks.first_bytes(64).1;
        (lerc > super::lerc::MAX_CHUNK_BYTES as u64).then_some(((chunks.cw, chunks.ch), lerc))
    } else {
        let tiff = (chunks.cw.min(w), if chunks.planes > 1 { chunks.ch } else { chunks.ch.min(h) });
        (cropped > decoding_limit() as u64 && (!lzw_reads || padded > super::lzw::MAX_CHUNK_BYTES as u64)).then_some((tiff, cropped))
    };
    if let Some(((cols, rows), bytes)) = refused {
        return Err(err(format!("a strip or tile of {cols} × {rows} pixels is too large to read ({} MB)", bytes >> 20)));
    }
    let mut bands = (0..nbands).map(|_| super::filled(ow, oh, f32::NAN)).collect::<Result<Vec<_>>>()?;
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((open, image, limits)) = open
        && examined >= PARALLEL_CHUNKS
    {
        // Each thread decodes a chunk of its own. Up to the default count as before; past it (a
        // high setting) no more than the threads' scratch budget holds, so the peak doesn't
        // multiply with the setting.
        let affordable = 1 + crate::memory::thread_scratch() / chunks.bytes().max(1);
        let chunk_count = usize::try_from(examined).unwrap_or(usize::MAX);
        let threads = crate::threads::max().min(affordable.max(crate::threads::default_max())).min(chunk_count);
        if threads > 1 {
            return decimate_parallel(chunks, bands, k, span, nodata, threads, open, image, limits);
        }
    }
    let _ = open;
    crate::progress::begin(usize::try_from(examined).unwrap_or(usize::MAX));
    let mut data_type = "Float32";
    let mut rounded = vec![0u64; nbands];
    let mut table = super::lerc::Table::default();
    for p in 0..examined {
        crate::progress::check()?;
        if let Some((plane, index)) = chunks.nth(p, span, k).and_then(|i| chunks.wanted(i, k, span)) {
            let (buf, gaps) = read_chunk(dec, index, &mut table)?;
            let changed = rounded_bits(&buf, nodata);
            let (mut values, dt) = to_f32(buf, (ow, oh))?;
            super::lerc::blank(&mut values, gaps.as_deref());
            data_type = dt;
            chunks.place(&mut bands, &values, changed.as_deref(), &mut rounded, plane, index, k, span)?;
        }
        crate::progress::step(1)?;
    }
    crate::progress::check()?;
    Ok((bands, data_type, rounded))
}

/// [`decimate`] on `threads` threads, each with its own decoder taking the next chunk in turn.
#[cfg(not(target_arch = "wasm32"))]
fn decimate_parallel(
    chunks: Chunks,
    bands: Vec<Vec<f32>>,
    k: u32,
    span: Span,
    nodata: Uncounted,
    threads: usize,
    open: Opener<'_>,
    image: usize,
    limits: Limits,
) -> Result<(Vec<Vec<f32>>, &'static str, Vec<u64>)> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let (ow, oh) = (span.width(), span.height());
    let examined = chunks.count_in(span, k);
    crate::progress::begin(usize::try_from(examined).unwrap_or(usize::MAX));
    crate::progress::check()?;
    let probe = crate::progress::current();
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
        let _guard = crate::progress::install(probe.clone());
        if let Err(e) = crate::progress::check() {
            return fail(e);
        }
        let dec = open().map_err(err).and_then(|r| Decoder::new(r).map_err(err)).map(|d| d.with_limits(limits.clone()));
        let mut dec = match dec.and_then(|mut d| d.seek_to_image(image).map_err(err).map(|()| d)) {
            Ok(d) => d,
            Err(e) => return fail(e),
        };
        let mut table = super::lerc::Table::default();
        while !failed.load(Ordering::Relaxed) {
            if let Err(e) = crate::progress::check() {
                return fail(e);
            }
            let p = next.fetch_add(1, Ordering::Relaxed);
            if p >= examined {
                break;
            }
            let Some((plane, index)) = chunks.nth(p, span, k).and_then(|i| chunks.wanted(i, k, span)) else {
                if let Err(e) = crate::progress::step(1) {
                    return fail(e);
                }
                continue;
            };
            match read_chunk(&mut dec, index, &mut table) {
                Ok((buf, gaps)) => {
                    let changed = rounded_bits(&buf, nodata);
                    let (mut values, dt) = match to_f32(buf, (ow, oh)) {
                        Ok(v) => v,
                        Err(e) => return fail(e),
                    };
                    super::lerc::blank(&mut values, gaps.as_deref());
                    let placed = match out.lock() {
                        Ok(mut o) => {
                            let (bands, data_type, rounded) = &mut *o;
                            *data_type = dt;
                            chunks.place(bands, &values, changed.as_deref(), rounded, plane, index, k, span)
                        }
                        Err(_) => Err(err("a chunk couldn't be decoded")),
                    };
                    if let Err(e) = placed {
                        return fail(e);
                    }
                }
                Err(e) => return fail(e),
            }
            if let Err(e) = crate::progress::step(1) {
                return fail(e);
            }
        }
    };
    std::thread::scope(|scope| {
        // A thread the OS refuses costs speed, not the read: this one decodes chunks too.
        let handles: Vec<_> = (1..threads).filter_map(|_| std::thread::Builder::new().spawn_scoped(scope, work).ok()).collect();
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
            fail(err("a chunk couldn't be decoded"));
        }
        for h in handles {
            if h.join().is_err() {
                fail(err("a chunk couldn't be decoded"));
            }
        }
    });
    if let Some(e) = failure.into_inner().ok().flatten() {
        return Err(e);
    }
    crate::progress::check()?;
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
    read_limited_with_nodata(reader(r).map_err(err)?, name, max_samples(), None, sidecar_nodata, None)
}

/// [`read`] of a file asked for in `asked` (a WCS coverage requested in its own system, #2032):
/// a file that doesn't say its coordinate system, and whose coordinates aren't degrees, is read in
/// `asked`, and said. One that names a system is read in it, as [`read`] reads it.
pub fn read_asked_in<R: Read + Seek>(r: R, name: &str, asked: Crs) -> Result<Raster> {
    read_limited_with_nodata(reader(r).map_err(err)?, name, max_samples(), None, None, Some(asked))
}

/// A single-band `UInt32` GeoTIFF of packed colour, each pixel `0xAABBGGRR` (alpha, blue, green
/// and red bytes, as GeoLibre's colour tools write a composite), as four `UInt8` bands, red,
/// green, blue and alpha, exact: read as one band (`grey`, [`read`] of `bytes`) its values are
/// held as `f32`, whose 24 bits drop the red byte (#2594). A pixel whose alpha is 0, that is the
/// file's no-data value (compared exactly), or that its transparency mask hides is see-through.
/// `None` when it isn't packed colour: not one `UInt32` band, or a pixel left visible whose alpha
/// byte is neither 0 nor 255, or none at 255 (a band of counts or IDs). `grey` tells which files
/// to decode again; their raw values decide. A rotated or skewed file (read resampled north up),
/// one without coordinates (a stand-in place), or one too large to hold four bands of, is an
/// error, for the caller to say. The image is
/// decoded once more, on one thread: at the peak, `grey`, the packed `u32`s and the four bands,
/// six times the band. What `grey` lost as it was read (its `lost`) is left for the caller to say.
pub fn unpack_rgba(grey: &Raster, bytes: &[u8]) -> Result<Option<Raster>> {
    let [band] = &grey.bands[..] else { return Ok(None) };
    if grey.data_type != "UInt32" {
        return Ok(None);
    }
    // As `f32`, values under 2^24 are exact and only 0xFEFFFF80–0xFEFFFFFF round up to
    // 0xFF000000: the raw values below settle those, and no-data. A value in between is no
    // colour unless it is no-data; any value at 0xFF000000 or past may be an opaque one.
    let between = |v: f32| (16_777_216.0..4_278_190_080.0).contains(&v);
    let may_be_packed = !band.data.iter().any(|v| between(*v) && grey.is_valid(*v)) && band.data.iter().any(|v| *v >= 4_278_190_080.0);
    if !may_be_packed {
        return Ok(None);
    }
    let mut dec = Decoder::new(reader(std::io::Cursor::new(bytes)).map_err(err)?).map_err(err)?.with_limits(limits());
    let (w, h) = dec.dimensions().map_err(err)?;
    if geo_tags(&mut dec)?.affine.is_some() {
        return Err(err("it is rotated or skewed, which packed colours aren't split for"));
    }
    let n = (w as usize).saturating_mul(h as usize);
    // An overview's pixels aren't the image's: its colours come from the whole image, as four bands.
    if grey.overview.is_some() || (grey.width, grey.height) != (w, h) || n.saturating_mul(4) > max_samples() {
        return Err(err(format!(
            "its {w} × {h} pixels are too many to hold as four bands: clip the tool's inputs to a smaller area first"
        )));
    }
    let DecodingResult::U32(packed) = dec.read_image().map_err(err)? else { return Ok(None) };
    if packed.len() != n || band.data.len() != n {
        return Err(err(format!("its image holds {} values, not {w} × {h}", packed.len())));
    }
    // No-data, exactly, or hidden by the file's transparency mask (NaN as read).
    let hole = |i: usize, p: u32| grey.nodata == Some(f64::from(p)) || band.data.get(i).is_some_and(|v| v.is_nan());
    let shown = || packed.iter().enumerate().filter(|(i, p)| !hole(*i, **p)).map(|(_, p)| p >> 24);
    if !shown().all(|a| a == 0 || a == 255) || !shown().any(|a| a == 255) {
        return Ok(None);
    }
    // A file with no GeoTIFF tags has only a stand-in place, which the rebuilt raster would
    // hold, and be written with, as coordinates.
    if !grey.georeferenced {
        return Err(err("it has no coordinates, and packed colours are split only with them"));
    }
    let (cols, rows) = (w as usize, h as usize);
    let channel = |shift: u32| super::collected(cols, rows, packed.iter().map(|p| ((p >> shift) & 0xFF) as f32));
    let opacity = packed.iter().enumerate().map(|(i, &p)| if hole(i, p) { 0.0 } else { (p >> 24) as f32 });
    let alpha = super::collected(cols, rows, opacity)?;
    let bands = vec![channel(0)?, channel(8)?, channel(16)?, alpha];
    let mut rgba = Raster::new(grey.name.clone(), w, h, bands, grey.transform, grey.crs, None, ColorHint::Rgb { alpha: true }, "UInt8")?;
    for (b, name) in rgba.bands.iter_mut().zip(["Red", "Green", "Blue", "Alpha"]) {
        b.name = name.into();
    }
    rgba.copyright.clone_from(&grey.copyright);
    Ok(Some(rgba))
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
    let file = crate::provenance::open(path).map_err(err)?;
    let revision = file.metadata().and_then(|metadata| super::file_revision::FileRevision::capture(&metadata));
    read_file_with_revision(path, name, sidecar_nodata, file, revision)
}

/// [`read_file_with_nodata`] of `part` of the image only, at the finest level whose part of it
/// fits `max` samples, decoding only the chunks that hold it (see [`read_part`]): `Ok(None)` when
/// the file can't be read by part, or isn't the revision `same_as` was read from. The raster's
/// pixels aren't the file's whole grid, so it keeps no revision of the file to pair later reads of
/// the grid with.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn read_file_part(
    path: &std::path::Path,
    name: &str,
    sidecar_nodata: Option<f64>,
    part: super::Part,
    max: usize,
    same_as: Option<&Raster>,
) -> Result<Option<super::PartRead>> {
    let file = crate::provenance::open(path).map_err(err)?;
    let revision = file.metadata().and_then(|metadata| super::file_revision::FileRevision::capture(&metadata));
    // The part is of the image `same_as` was read from: of another, it would be another place.
    // The revision compared is the one the read is checked against as it goes.
    if let Some(r) = same_as
        && !revision.as_ref().is_ok_and(|rev| r.is_from(rev))
    {
        return Ok(None);
    }
    read_file_revision_part(path, name, sidecar_nodata, file, revision, Some(part), max)
}

#[cfg(not(target_arch = "wasm32"))]
fn load_revision_error(reason: impl std::fmt::Display) -> String {
    format!(
        "{reason}. Wait until the file is stable, then Retry Add Data (for an existing layer, Reload Data). Or copy it to a stable local location and add the copy (for an existing layer, point it at the copy with Repair Data Source)."
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn read_file_with_revision(
    path: &std::path::Path,
    name: &str,
    sidecar_nodata: Option<f64>,
    file: std::fs::File,
    revision: std::io::Result<super::file_revision::FileRevision>,
) -> Result<Raster> {
    read_file_revision_part(path, name, sidecar_nodata, file, revision, None, max_samples())?
        .map(|p| p.raster)
        .ok_or_else(|| err("the image couldn't be read"))
}

/// [`read_file_with_revision`], of `part` of the image only when given (`Ok(None)` when the file
/// can't be read by part, see [`read_part`]).
#[cfg(not(target_arch = "wasm32"))]
fn read_file_revision_part(
    path: &std::path::Path,
    name: &str,
    sidecar_nodata: Option<f64>,
    file: std::fs::File,
    revision: std::io::Result<super::file_revision::FileRevision>,
    part: Option<super::Part>,
    max: usize,
) -> Result<Option<super::PartRead>> {
    let revision = revision.map_err(|e| e.to_string());
    let open = || -> std::io::Result<Reader<'static>> {
        let file = crate::provenance::open(path)?;
        if let Ok(original) = &revision {
            let current = file
                .metadata()
                .and_then(|metadata| super::file_revision::FileRevision::capture(&metadata))
                .map_err(|e| std::io::Error::other(load_revision_error(format!("Could not verify the image file while reading: {e}"))))?;
            if *original != current {
                return Err(std::io::Error::other(load_revision_error("The image file changed while its pixels were read")));
            }
        }
        reader(std::io::BufReader::new(file))
    };
    let decoded =
        reader(std::io::BufReader::new(file)).map_err(err).and_then(|r| read_image(r, name, max, Some(&open), sidecar_nodata, None, part));
    // A file cut short while it is read fails to decode: the revision says whether that was a
    // change to wait out (Cancel is said as itself).
    if let Ok(original) = &revision
        && !matches!(decoded, Err(crate::DataError::Cancelled))
    {
        let current = std::fs::metadata(path)
            .and_then(|metadata| super::file_revision::FileRevision::capture(&metadata))
            .map_err(|e| err(load_revision_error(format!("Could not verify the image file after reading: {e}"))))?;
        if *original != current {
            return Err(err(load_revision_error("The image file changed while its pixels were read")));
        }
    }
    let Some(mut read) = decoded? else { return Ok(None) };
    // A part's pixels aren't the file's grid: no later read of it pairs with them.
    if part.is_none() {
        read.raster.source_file = Some(revision);
    }
    Ok(Some(read))
}

/// [`read`], holding at most `max` samples (the tests' small limit exercises the overviews), with
/// `open` making more readers of the same file for decoding it on several threads.
pub(super) fn read_limited(r: Reader<'_>, name: &str, max: usize, open: Option<Opener<'_>>) -> Result<Raster> {
    read_limited_with_nodata(r, name, max, open, None, None)
}

/// [`read_limited`], with the no-data value a sidecar gives (`.aux.xml`, applied after reading by
/// [`Raster::apply_pam`]): when the file has none of its own, its pixels aren't counted as rounded.
pub(super) fn read_limited_with_nodata(
    r: Reader<'_>,
    name: &str,
    max: usize,
    open: Option<Opener<'_>>,
    sidecar_nodata: Option<f64>,
    asked: Option<Crs>,
) -> Result<Raster> {
    // Only a part is ever not read.
    read_image(r, name, max, open, sidecar_nodata, asked, None)?.map(|p| p.raster).ok_or_else(|| err("the image couldn't be read"))
}

/// [`read_limited_with_nodata`] of `part` of the image only: the finest level (the full image, an
/// overview, or every `k`-th pixel of the full image when it has none fine enough) whose part
/// fits `max`, its part read out to whole pixels, decoding only the strips or tiles that hold it.
/// Its transparency mask is read for the part alike. `Ok(None)` when the image can't be read by
/// part: a rotated or skewed one (read resampled north up), or one without coordinates (placed,
/// if at all, by a world file once read). Its no-data value, coordinate system and band
/// descriptions are the whole image's. `overview`, when the level isn't the full image, is the
/// part's size in the full image's pixels.
#[cfg(test)]
pub(super) fn read_part(r: Reader<'_>, name: &str, max: usize, part: super::Part) -> Result<Option<super::PartRead>> {
    read_image(r, name, max, None, None, None, Some(part))
}

/// [`read_limited_with_nodata`], or of `part` of the image only ([`read_part`]).
fn read_image(
    r: Reader<'_>,
    name: &str,
    max: usize,
    open: Option<Opener<'_>>,
    sidecar_nodata: Option<f64>,
    asked: Option<Crs>,
    part: Option<super::Part>,
) -> Result<Option<super::PartRead>> {
    // The full image says whether it is read as a palette image, whichever image is read, as a
    // COG window has it: a grey full image's stray colour table colours nothing, nor do palette
    // overviews of it.
    let palette = r.first_is_palette();
    let wide_rgb = r.first_is_wide_rgb();
    let limits = limits();
    let mut dec = Decoder::new(r).map_err(err)?.with_limits(limits.clone());
    let (full_w, full_h) = dec.dimensions().map_err(err)?;
    let geo = geo_tags(&mut dec)?;
    // A turned image is resampled north up whole, and one without coordinates may be placed by a
    // world file: neither is read by part.
    if part.is_some() && (geo.affine.is_some() || !geo.referenced) {
        return Ok(None);
    }
    // The no-data value whose pixels aren't counted as rounded.
    let uncounted = Uncounted::new(geo.nodata_exact, sidecar_nodata);
    // The bands' descriptions GDAL keeps in the file (an `.aux.xml` sidecar's are applied after).
    // A tag that doesn't decode is left out, and said (`Raster::lost`).
    let described = match dec.find_tag(Tag::Unknown(42112)) {
        Ok(None) => None,
        Ok(Some(v)) => match v.into_string() {
            Ok(t) => super::pam::parse_gdal_metadata(&t),
            Err(e) => Some(super::pam::Pam::losing(format!("it isn't text ({e}): band and class names and wavelengths are left out"))),
        },
        Err(e) => Some(super::pam::Pam::losing(format!("it doesn't decode ({e}): band and class names and wavelengths are left out"))),
    };
    // Who holds the rights to the picture, kept as it reads so an export of it says so too (the
    // writers make it the ASCII a tag holds, and the exports say what that changed); one that
    // doesn't read as text is left out, and said.
    let (copyright, copyright_lost) = match copyright_tag(&mut dec) {
        Ok(c) => (c, None),
        Err(e) => (None, Some(e)),
    };
    let spp = samples(&mut dec);
    let full_alpha = extra_alpha(&mut dec);
    let bytes = sample_bytes(&mut dec);
    // The decoded image is held at its own pixel size before conversion to f32, so bound bytes
    // as well as samples.
    // `w` × `h` pixels whose image spans `extent` of them each way (every k-th pixel's grid
    // passes the image's edge when k doesn't divide its size).
    let fits = |w: u32, h: u32, extent: (f64, f64), spp: usize, bytes: usize| {
        // Of a part, its pixels at that size.
        let (w, h) = part.map_or((w, h), |p| p.span((w, h), extent).size());
        let n = (w as usize).saturating_mul(h as usize).saturating_mul(spp);
        n <= max && n.saturating_mul(bytes) <= max.saturating_mul(4)
    };
    let (mut w, mut h) = (full_w, full_h);
    let mut overview = None;
    // Every k-th pixel of the full image, for a large one without overviews (1: read whole).
    let mut decimate_by = 1u32;
    // The image read: 0, the full one, or an overview.
    let mut image = 0;
    if !fits(w, h, (f64::from(w), f64::from(h)), spp, bytes) {
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
            // An overview with another sample count (three of an RGBI image's four), or another
            // alpha role for the sample past the colour ones (an RGBA overview of an RGBI image),
            // would read as another kind of image: the full image is read sparsely instead.
            if ospp == spp
                && extra_alpha(&mut dec) == full_alpha
                && fits(ow, oh, (f64::from(ow), f64::from(oh)), ospp, obytes)
                && best.is_none_or(|(_, bw, bh)| ow as u64 * oh as u64 > bw as u64 * bh as u64)
            {
                best = Some((index, ow, oh));
            }
        }
        // Every k-th pixel of the full image, the fewest left out that fit.
        let mut k = 2u32;
        while !fits(full_w.div_ceil(k), full_h.div_ceil(k), (f64::from(full_w) / f64::from(k), f64::from(full_h) / f64::from(k)), spp, 4) {
            k = k.saturating_mul(2);
            if k > 1 << 20 {
                break;
            }
        }
        let (dw, dh) = (full_w.div_ceil(k), full_h.div_ceil(k));
        let decimated = (k <= 1 << 20).then_some(u64::from(dw) * u64::from(dh));
        // As detailed as every k-th pixel is an overview of the full size over k, rounded either
        // way: GDAL rounds up, some writers down (terrio's 10439 × 9842 for 83517 × 78739 over
        // 8, #1965), and one a pixel short must not make the whole full image be read instead.
        let as_detailed = |pixels: u64| pixels >= u64::from((full_w / k).max(1)) * u64::from((full_h / k).max(1));
        match best {
            // An overview as detailed, to the rounding of its size, as the full image read
            // sparsely: only it is read.
            Some((index, ow, oh)) if decimated.is_none() || as_detailed(u64::from(ow) * u64::from(oh)) => {
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
        // An RGB image with more bands (shown to the decoder as grey, see `Unpaletted`): every
        // band, the first three red, green and blue.
        ColorType::Multiband { num_samples, .. } if wide_rgb && num_samples >= 3 => {
            (num_samples as usize, ColorHint::Rgb { alpha: extra_alpha(&mut dec) })
        }
        ColorType::Multiband { num_samples, .. } => (num_samples.max(1) as usize, multiband_color(&mut dec, num_samples)),
        other => return Err(err(format!("{other:?} images aren't supported"))),
    };
    let planar = dec.find_tag_unsigned::<u16>(Tag::PlanarConfiguration).ok().flatten() == Some(2);
    // The pixels kept of the grid read (`w` × `h`): a part's, else all of them.
    // The image's extent in the grid's pixels: every k-th pixel's grid passes its edge when k
    // doesn't divide its size.
    let extent = if decimate_by > 1 {
        (f64::from(full_w) / f64::from(decimate_by), f64::from(full_h) / f64::from(decimate_by))
    } else {
        (f64::from(w), f64::from(h))
    };
    let span = part.map(|p| p.span((w, h), extent));
    let (ow, oh) = span.map_or((w, h), Span::size);
    let n = (ow as usize) * (oh as usize);
    // Extra samples the decoder leaves out can make padded edge tiles spill into the next row
    // when read whole (issue #162). Read those tile by tile. An RGBI image the patched reader
    // shows as grey (see `Unpaletted`) keeps its fourth sample and doesn't need this.
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
    // The decoder has no LERC codec: such an image is read a chunk at a time, every chunk decoded
    // by [`super::lerc`] (#1818).
    let lerc = super::lerc::is_lerc(&mut dec);
    let whole = if decimate_by > 1 || extra_samples || planar_tiles || chunked || lerc || span.is_some() {
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
        let (values, dt) = to_f32(buf, (ow as usize, oh as usize))?;
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
                let mut band = super::room(n, ow as usize, oh as usize)?;
                band.extend_from_slice(plane);
                bands.push(band);
            }
            bands
        } else {
            // One pass over the interleaved pixels.
            let mut bands = (0..nbands).map(|_| super::room(n, ow as usize, oh as usize)).collect::<Result<Vec<Vec<f32>>>>()?;
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
        decimate(&mut dec, iw, ih, nbands, planar, decimate_by, span, uncounted, open)?
    };
    let rounded = if rounded.iter().any(|n| *n > 0) { rounded } else { Vec::new() };

    if ycbcr {
        ycbcr_to_rgb(&mut bands);
    }
    // The image's internal transparency mask: its masked pixels are no-data, as outside a
    // reprojection are. One that can't be read leaves them as stored, and says so.
    let mut mask_lost = None;
    match super::mask::find(&mut dec, image, if decimate_by > 1 { (full_w, full_h) } else { (w, h) }) {
        super::mask::Found::At(m) => match super::mask::read(&mut dec, m, decimate_by, span) {
            Ok(opaque) if opaque.len() == n => {
                for band in &mut bands {
                    for (v, _) in band.iter_mut().zip(&opaque).filter(|(_, o)| !**o) {
                        *v = f32::NAN;
                    }
                }
            }
            Ok(_) => mask_lost = Some("its transparency mask doesn't match its image".to_string()),
            Err(DataError::Cancelled) => return Err(DataError::Cancelled),
            Err(e) => mask_lost = Some(e.to_string()),
        },
        super::mask::Found::Elsewhere => mask_lost = Some("the file has none for the image read".to_string()),
        super::mask::Found::Unscanned(why) => mask_lost = Some(why),
        super::mask::Found::None => {}
    }
    let mask_lost = mask_lost.map(|why| format!("its transparency mask isn't applied ({why}): masked pixels draw as stored"));
    // A scan keeps a stand-in transform, sized to the pixels read.
    if !geo.referenced {
        let mut r = Raster::new(name, w, h, bands, super::unreferenced(w, h), Crs::LonLat, geo.nodata, color, data_type)?;
        r.georeferenced = false;
        r.overview = overview;
        r.step = decimate_by.max(1);
        r.rounded = rounded;
        r.copyright = copyright.filter(|c| !c.is_empty());
        r.lost.extend(copyright_lost);
        r.lost.extend(table_lost);
        r.lost.extend(mask_lost);
        if let Some(p) = &described {
            r.apply_pam(p, super::pam::GDAL_METADATA);
        }
        let (grid, bounds) = ((r.transform, w, h), r.extent());
        return Ok(Some(super::PartRead { raster: r, at: (0, 0), grid, bounds, full: (full_w, full_h) }));
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
    let mut turned = None;
    let (bands, w, h) = match geo.affine {
        Some([x, xc, xr, y, yc, yr]) => {
            let affine = [x, xc * sx, xr * sy, y, yc * sx, yr * sy];
            let (bands, ow, oh, north_up) = super::north_up(&bands, w, h, affine, max)?;
            // The file's own placement, not the scaled one: its pixels are the ones it is coarser than.
            turned = Some(turned_note([x, xc, xr, y, yc, yr], (ow, oh), &north_up));
            transform = north_up;
            (bands, ow, oh)
        }
        None => (bands, w, h),
    };
    // What makes a system the file doesn't give one read as WGS 84, said once.
    let mut assumed = None;
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
            if let Some(asked) = asked.filter(|_| geo.custom_error.is_none() && !geo.geographic_model && !degrees) {
                assumed = Some(format!("it doesn't say its coordinate system: read in {}, the system it was asked for in", asked.label()));
                asked
            } else if geo.geographic_model || degrees {
                assumed = Some(match &geo.custom_error {
                    // The file's own projection couldn't be read, but its coordinates fit in degrees:
                    // why is said, not dropped.
                    Some(e) => super::assumed_wgs84_note(&format!(
                        "its own coordinate system can't be read ({})",
                        e.strip_prefix("GeoTIFF: ").unwrap_or(e)
                    )),
                    None if geo.geographic_model => NO_EPSG_GEOGRAPHIC.to_string(),
                    None => super::assumed_wgs84_note("it doesn't say its coordinate system"),
                });
                Crs::LonLat
            } else {
                return Err(err("the file's coordinate system isn't recognised"));
            }
        }
    };
    // The grid read, whole, as a whole read of it places it.
    let grid = (transform, w, h);
    // A part: its corner, and its size in the full image's pixels when it is coarser.
    let (w, h, overview) = match span {
        Some(span) => {
            transform.origin_x += f64::from(span.cols.0) * transform.pixel_width;
            transform.origin_y += f64::from(span.rows.0) * transform.pixel_height;
            let file = |n: u32, by: f64| (f64::from(n) * by).round().clamp(1.0, f64::from(u32::MAX)) as u32;
            (ow, oh, overview.map(|_| (file(ow, sx), file(oh, sy))))
        }
        None => (w, h, overview),
    };
    let mut r = Raster::new(name, w, h, bands, transform, crs, geo.nodata, color, data_type)?;
    r.overview = overview;
    // A turned image is resampled to a grid of its own: its pixels are no longer the file's.
    r.step = if geo.affine.is_none() { decimate_by.max(1) } else { 1 };
    r.rounded = rounded;
    r.copyright = copyright.filter(|c| !c.is_empty());
    r.lost.extend(copyright_lost);
    r.lost.extend(super::planetary::note_of(crs));
    r.crs_assumed = assumed.is_some();
    r.lost.extend(assumed);
    r.lost.extend(turned);
    r.lost.extend(table_lost);
    r.lost.extend(mask_lost);
    if let Some(p) = &described {
        r.apply_pam(p, super::pam::GDAL_METADATA);
    }
    let at = span.map_or((0, 0), |s| (s.cols.0, s.rows.0));
    let bounds = if span.is_some() { super::extent_of(&grid.0, r.transformer(), grid.1, grid.2) } else { r.extent() };
    Ok(Some(super::PartRead { raster: r, at, grid, bounds, full: (full_w, full_h) }))
}

/// What turning a rotated or skewed image north up says ([`super::north_up`]): its pixels are
/// resampled, nearest pixel, onto square ones of the same area, coarser when the grid would pass the
/// sample limit. `affine` is the file's own, at full resolution, `(w, h)` the grid's size and `grid`
/// its placement.
fn turned_note(affine: [f64; 6], (w, h): (u32, u32), grid: &GeoTransform) -> String {
    let [_, xc, xr, _, yc, yr] = affine;
    let own = (xc * yr - xr * yc).abs().sqrt();
    // Four significant digits.
    let short = |v: f64| {
        let digits = if v.is_finite() && v != 0.0 { (3 - v.abs().log10().floor() as i32).clamp(0, 15) } else { 0 };
        format!("{v:.*}", digits as usize)
    };
    let mut note = format!(
        "it is rotated or skewed, so it is resampled (nearest pixel) onto a north-up grid of {w} × {h} pixels of {} each; \
         values are taken from source pixel centres, and the grid's corners beyond the image are empty",
        short(grid.pixel_width)
    );
    if grid.pixel_width > own * (1.0 + 1e-9) {
        note.push_str(&format!(", and its pixels are coarser than the file's ({}) to keep the grid within the sample limit", short(own)));
    }
    note
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
            && let Some((geokeys, doubles, ascii)) = terrio_keys(&d)
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
        // The tiepoint is written at the corner of the first pixel, so the raster type is area: a
        // system first read from a point file (whose grid was moved to the corner on reading) is
        // written as one too, or readers would place it half a pixel off.
        Some(d) => as_area(&d.geokeys),
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

/// GeoKey directory `keys` with the raster type (1025) as area (1), as the writers' corner
/// tiepoint says.
fn as_area(keys: &[u16]) -> Vec<u16> {
    let mut keys = keys.to_vec();
    // After the 4-short header, 4 shorts a key: its id, where its value is (0: in place), its
    // count and its value.
    for entry in keys.get_mut(4..).into_iter().flat_map(|k| k.as_chunks_mut::<4>().0) {
        if let [1025, 0, _, value] = entry {
            *value = 1;
        }
    }
    keys
}

/// Whether band `b` is written as bytes: whole numbers within 0–255 (or no valid pixels at all),
/// and a no-data value that is one too. A colour band's statistics leave out the cells its alpha
/// draws see-through (#1810), so those cells' values are looked at too: a file is written in the
/// type it was before, whatever they hold.
pub(super) fn fits_bytes(r: &Raster, b: usize) -> bool {
    let Some(band) = r.bands.get(b) else { return false };
    let s = &band.stats;
    let whole_bytes = s.min >= 0.0 && s.max <= 255.0 && (s.categories.is_some() || r.data_type == "UInt8");
    let nodata_ok = r.nodata.is_none_or(|d| (0.0..=255.0).contains(&d) && d.fract() == 0.0);
    let byte = |v: f32| !r.is_valid(v) || (v.fract() == 0.0 && (0.0..=255.0).contains(&v));
    // Looked at only once the statistics fit, as it reads the whole band; an 8-bit band's are
    // bytes already.
    let see_through_bytes = || {
        r.data_type == "UInt8"
            || r.see_through_cells(b).is_none_or(|hidden| band.data.iter().enumerate().all(|(i, &v)| !hidden(i) || byte(v)))
    };
    nodata_ok && (whole_bytes || s.count == 0) && see_through_bytes()
}

/// The bands [`write`] writes of `r` given `band`: RGB and its fourth sample (alpha only when
/// declared alpha), otherwise `band` alone.
pub fn written_bands(r: &Raster, band: usize) -> Vec<usize> {
    if !(matches!(r.color, ColorHint::Rgb { .. }) && r.bands.len() >= 3) {
        return vec![band];
    }
    let fourth = r.bands.len() == 4 || (r.bands.len() > 4 && matches!(r.color, ColorHint::Rgb { alpha: true }));
    (0..if fourth { 4 } else { 3 }).collect()
}

/// A cell `v` of `r` as a float file holds it beside an alpha band. A float file's alpha is opaque
/// at 1, so an integer raster's alpha (`is_alpha`) is divided by its `opaque` value (see
/// [`super::alpha_max`]). A cell with no value stays without one: as it is, or as NaN when
/// `nan_cells` (see [`alpha_hits_no_data`]).
fn float_cell(r: &Raster, opaque: f32, nan_cells: bool, is_alpha: bool, v: f32) -> f32 {
    if !r.is_valid(v) {
        if nan_cells { f32::NAN } else { v }
    } else if is_alpha {
        v / opaque
    } else {
        v
    }
}

/// Whether a valid cell of `alpha`, divided by `opaque`, would become the no-data value the file's
/// tag names (an opaque alpha is 1; a no-data value of 1, or of 0.5 and a half-see-through alpha
/// of the right size). Then no-data is written as NaN, with the tag `nan`, in every band.
fn alpha_hits_no_data(r: &Raster, opaque: f32, alpha: &[f32]) -> bool {
    // An alpha already on the float scale (`opaque` 1: a float raster, or no declared alpha) isn't
    // changed, so can't collide: no scan.
    r.nodata.is_some() && opaque != 1.0 && alpha.iter().any(|&v| r.is_valid(v) && !r.is_valid(v / opaque))
}

/// Writes a raster as a Deflate-compressed GeoTIFF: a colour raster's colour bands (RGB(A) bytes
/// when they are 8-bit, else 32-bit floats, alpha kept), so it reads back in colour; otherwise
/// `band` (0-based), as bytes when it is an 8-bit band of whole bytes, else as 32-bit floats.
/// An integer raster's alpha, when written as floats, is divided by its opaque value (1 is opaque
/// in a float file; see [`float_cell`]). `band` is for rasters that aren't colour ones: a colour raster always writes its colour bands,
/// as its 8-bit form did before. The strips go through the predictors and Deflate effort of the
/// COG writer ([`super::cogwrite::write_strips`]). A cancelled run stops it (with
/// [`DataError::Cancelled`]) within a strip.
pub fn write(r: &Raster, band: usize) -> Result<Vec<u8>> {
    encode(r, band, false)
}

/// Writes every band of a raster, for a layer whose pixels the project keeps: as [`write`] does
/// when that keeps them all. Otherwise: 8-bit samples when the raster is `UInt8` and every valid
/// value is a whole number in 0–255 (an empty cell only where no-data can mark it), else 32-bit
/// float samples (RGB first for a colour raster, and alpha after them when it has one). [`read`]
/// reads them back as the same bands. An integer raster's alpha, when written as floats, is divided
/// by its opaque value, as [`write`] does.
pub fn write_all(r: &Raster) -> Result<Vec<u8>> {
    encode(r, 0, true)
}

/// Writes `data` (samples interleaved as `img` lays them out, a row after another) as `img`'s
/// strips, at the end of `out`.
fn put<T: Copy + Sync, const N: usize>(
    img: super::cogwrite::Strips,
    data: &[T],
    le: fn(T) -> [u8; N],
    out: &mut std::io::Cursor<Vec<u8>>,
) -> Result<()> {
    let per_row = img.w as usize * usize::from(img.spp);
    super::cogwrite::write_strips(img, out, |rows, buf| {
        let s = data.get(rows.start * per_row..rows.end * per_row).ok_or_else(|| err("the samples don't fill the image"))?;
        buf.extend(s.iter().flat_map(|v| le(*v)));
        Ok(())
    })
}

/// [`write`] (or with `every`, [`write_all`]).
fn encode(r: &Raster, band: usize, every: bool) -> Result<Vec<u8>> {
    use super::cogwrite::{Sample, Strips, few_classes, geo_entries};
    // A scan's place is only where it was laid on the view: not coordinates to write into a file.
    r.require_coordinates().map_err(err)?;
    let mut out = std::io::Cursor::new(Vec::new());
    let gt = r.transform;
    if gt.pixel_height > 0.0 || gt.pixel_width < 0.0 {
        return Err(err("only north-up rasters can be exported for now"));
    }
    let geo = geo_keys(r)?;
    let (w, h) = (r.width, r.height);
    // Every band holds width × height values; one that doesn't is refused rather than padded.
    if r.bands.iter().any(|b| b.data.len() != (w as usize) * (h as usize)) {
        return Err(err(format!("a band of the raster doesn't hold its {w} × {h} values")));
    }
    let bytes = |b: usize| -> Option<Vec<u8>> {
        let band = r.bands.get(b)?;
        // No-data pixels keep the no-data value, so the written GDAL_NODATA tag still matches.
        let nodata = r.nodata.map(|d| d as u8);
        fits_bytes(r, b)
            .then(|| band.data.iter().map(|v| if r.is_valid(*v) { v.clamp(0.0, 255.0) as u8 } else { nodata.unwrap_or(0) }).collect())
    };
    let rgb = matches!(r.color, ColorHint::Rgb { .. }) && r.bands.len() >= 3;
    // The image each branch below writes: `written` names the bands it writes as samples, whose
    // names, and the first one's class names (the red band's, for a colour file), go to GDAL's
    // metadata tag, read back on open. Grey or RGB samples come first, then those `extra` says
    // what they are (unassociated alpha, 2, or data, 0). A single band of classes takes no
    // predictor.
    // Set when no-data is written as NaN (see [`alpha_hits_no_data`]): the tag then says so.
    let nan_tag = std::cell::Cell::new(false);
    let nodata_tag = || if nan_tag.get() { Some("nan".to_string()) } else { r.nodata.map(|d| d.to_string()) };
    let image = |written: &[usize], sample: Sample, extra: Vec<u16>| -> Result<Strips> {
        let colour = if rgb { 3 } else { 1 };
        Ok(Strips {
            w,
            h,
            spp: u16::try_from(colour + extra.len()).map_err(|_| err("a TIFF holds at most 65,535 bands"))?,
            sample,
            photometric: if rgb { 2 } else { 1 },
            extra,
            classes: matches!(written, [b] if r.bands.get(*b).is_some_and(few_classes)),
            tags: geo_entries(r, &geo, written, nodata_tag()),
        })
    };
    // The fourth sample of a colour file says what it is: alpha, or data (as RGB and near-infrared).
    let fourth = || vec![if matches!(r.color, ColorHint::Rgb { alpha: true }) { 2 } else { 0 }];
    // Without alpha or a no-data marker, byte samples cannot keep NaN holes (for example the
    // cells outside a reprojected RGBI image). Keep those samples as floats instead. With alpha,
    // a hole its alpha hides is fine (the cell is see-through), but one under an opaque alpha
    // would be written as an opaque 0. A pass over the bands: the byte branch asks it last.
    let byte_holes = || {
        let counted =
            || written_bands(r, band).iter().any(|b| r.bands.get(*b).is_some_and(|b| b.stats.count < (w as usize) * (h as usize)));
        r.nodata.is_none()
            && match (&r.color, r.color.alpha(r.bands.len())) {
                (ColorHint::Rgb { alpha: false }, _) => counted(),
                (ColorHint::Rgb { alpha: true }, Some((_, colour))) => colour.into_iter().any(|b| {
                    r.bands
                        .get(b)
                        .zip(r.see_through_cells(b))
                        .is_some_and(|(band, hidden)| band.data.iter().enumerate().any(|(i, v)| !v.is_finite() && !hidden(i)))
                }),
                // No alpha band to hide a hole (alpha declared on three bands).
                (ColorHint::Rgb { alpha: true }, None) => counted(),
                _ => false,
            }
    };
    let all: Vec<usize> = (0..r.bands.len()).collect();
    if every && written_bands(r, band).len() < all.len() {
        // More bands than colour (or one grey band) takes: each a 32-bit float sample, the first
        // past the colour (or grey) ones alpha when the raster says it is.
        // Alpha follows the colour (or grey) samples it belongs to: RGB's is the fourth band, grey's
        // the second, so a colour hint on too few bands for colour marks none.
        let alpha = if rgb { matches!(r.color, ColorHint::Rgb { alpha: true }) } else { matches!(r.color, ColorHint::GrayAlpha) };
        let extras: Vec<u16> = (0..all.len() - if rgb { 3 } else { 1 }).map(|i| if i == 0 && alpha { 2 } else { 0 }).collect();
        // The samples interleaved, as large as the raster: reserved first, so a raster this
        // machine can't hold twice is an error, and a cancel is heard while they are copied.
        // Bytes when every band is whole bytes (and an empty cell can be written as no-data),
        // so an 8-bit image reads back as one; otherwise 32-bit floats.
        let n = (w as usize) * (h as usize);
        let samples = n.checked_mul(all.len()).ok_or_else(|| err("the raster is too large to write"))?;
        // Decided from the values, not the statistics (a raster read in part has none): every
        // valid value a whole number in 0–255, and an empty cell only where no-data can mark it.
        // An 8-bit raster can hold fractions after resampling; those stay floats.
        let in_bytes = |v: &f32| !r.is_valid(*v) || (v.fract() == 0.0 && (0.0..=255.0).contains(v));
        let marked = |v: &f32| r.nodata.is_some() || r.is_valid(*v);
        // Only an 8-bit raster: a float one holding byte-range values keeps its type and range.
        let as_bytes = r.data_type == "UInt8"
            && all.iter().all(|b| fits_bytes(r, *b))
            && r.bands.iter().all(|b| b.data.iter().all(|v| in_bytes(v) && marked(v)));
        macro_rules! wide {
            ($sample:expr, $kind:expr, $le:expr) => {{
                let mut data = super::room(samples, w as usize, h as usize)?;
                for i in 0..n {
                    if i % (1 << 20) == 0 {
                        crate::progress::check()?;
                    }
                    // Every band holds `n` values (checked above), so none is filled in.
                    data.extend(r.bands.iter().enumerate().map(|(bi, b)| $sample(bi, b.data.get(i).copied().unwrap_or(f32::NAN))));
                }
                put(image(&all[..], $kind, extras)?, &data, $le, &mut out)?;
            }};
        }
        if as_bytes {
            let nodata = r.nodata.map_or(0, |d| d as u8);
            let byte = |_: usize, v: f32| if r.is_valid(v) { v.clamp(0.0, 255.0) as u8 } else { nodata };
            wide!(byte, Sample::Byte, u8::to_le_bytes)
        } else {
            let (opaque, alpha_band) = (super::alpha_max(&r.data_type), alpha.then_some(if rgb { 3 } else { 1 }));
            let nan_cells = alpha_band.and_then(|a| r.bands.get(a)).is_some_and(|b| alpha_hits_no_data(r, opaque, &b.data));
            nan_tag.set(nan_cells);
            wide!(|bi: usize, v: f32| float_cell(r, opaque, nan_cells, Some(bi) == alpha_band, v), Sample::Float, f32::to_le_bytes)
        }
    } else if rgb
        && (!written_bands(r, band).contains(&3) || fits_bytes(r, 3))
        && let (Some(red), Some(green), Some(blue)) = (bytes(0), bytes(1), bytes(2))
        && !byte_holes()
    {
        let alpha = if written_bands(r, band).contains(&3) { bytes(3) } else { None };
        let n = (w as usize) * (h as usize);
        if let Some(a) = alpha {
            let mut data = Vec::with_capacity(n * 4);
            for i in 0..n {
                data.extend([red.get(i), green.get(i), blue.get(i), a.get(i)].map(|v| v.copied().unwrap_or(0)));
            }
            put(image(&[0, 1, 2, 3], Sample::Byte, fourth())?, &data, u8::to_le_bytes, &mut out)?;
        } else {
            let mut data = Vec::with_capacity(n * 3);
            for i in 0..n {
                data.extend([red.get(i), green.get(i), blue.get(i)].map(|v| v.copied().unwrap_or(0)));
            }
            put(image(&[0, 1, 2], Sample::Byte, Vec::new())?, &data, u8::to_le_bytes, &mut out)?;
        }
    } else if rgb && let (Some(red), Some(green), Some(blue)) = (r.bands.first(), r.bands.get(1), r.bands.get(2)) {
        // Colour bands that aren't bytes (three embedding dimensions): kept as 32-bit floats, with
        // their alpha band when they have one, scaled by [`float_cell`].
        let n = (w as usize) * (h as usize);
        let at = |b: &crate::raster::Band, i: usize| b.data.get(i).copied().unwrap_or(f32::NAN);
        // Only a declared alpha: the fourth band of an RGBI image is data.
        let opaque = if matches!(r.color, ColorHint::Rgb { alpha: true }) { super::alpha_max(&r.data_type) } else { 1.0 };
        match r.bands.get(3).filter(|_| written_bands(r, band).contains(&3)) {
            Some(alpha) => {
                let nan_cells = alpha_hits_no_data(r, opaque, &alpha.data);
                nan_tag.set(nan_cells);
                let cell = |is_alpha: bool, v: f32| float_cell(r, opaque, nan_cells, is_alpha, v);
                let mut data = Vec::with_capacity(n * 4);
                for i in 0..n {
                    data.extend([cell(false, at(red, i)), cell(false, at(green, i)), cell(false, at(blue, i)), cell(true, at(alpha, i))]);
                }
                put(image(&[0, 1, 2, 3], Sample::Float, fourth())?, &data, f32::to_le_bytes, &mut out)?;
            }
            None => {
                let mut data = Vec::with_capacity(n * 3);
                for i in 0..n {
                    data.extend([at(red, i), at(green, i), at(blue, i)]);
                }
                put(image(&[0, 1, 2], Sample::Float, Vec::new())?, &data, f32::to_le_bytes, &mut out)?;
            }
        }
    } else {
        let b = r.bands.get(band).ok_or_else(|| err(format!("the raster has no band {}", band + 1)))?;
        // An 8-bit band is written as bytes when every value is a whole byte and an empty cell is
        // only where no-data can mark it (as the multiband samples above); otherwise as floats.
        let byte = |v: &f32| if r.is_valid(*v) { v.fract() == 0.0 && (0.0..=255.0).contains(v) } else { r.nodata.is_some() };
        if r.data_type == "UInt8" && fits_bytes(r, band) && b.data.iter().all(byte) {
            let nodata = r.nodata.map_or(0, |d| d as u8);
            let mut data = super::room(b.data.len(), w as usize, h as usize)?;
            for (i, v) in b.data.iter().enumerate() {
                if i % (1 << 20) == 0 {
                    crate::progress::check()?;
                }
                data.push(if r.is_valid(*v) { *v as u8 } else { nodata });
            }
            put(image(&[band], Sample::Byte, Vec::new())?, &data, u8::to_le_bytes, &mut out)?;
        } else {
            put(image(&[band], Sample::Float, Vec::new())?, &b.data, f32::to_le_bytes, &mut out)?;
        }
    }
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::tests::small;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn annotation_grid_geotiff_open_failure_retains_format_context() {
        let dir = tempfile::tempdir().unwrap();
        let error = read_file(&dir.path().join("missing.tif"), "missing").unwrap_err().to_string();
        assert!(error.contains("GeoTIFF:"), "{error}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn annotation_grid_load_change_and_missing_path_refuse_with_add_data_recovery() {
        use std::io::Write;
        for removed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("image.tif");
            let pixels = small();
            let bytes = write(&pixels, 0).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            let file = std::fs::File::open(&path).unwrap();
            let revision = super::super::file_revision::FileRevision::capture(&file.metadata().unwrap()).unwrap();
            // Inject a change between the initial source snapshot and the final load check,
            // without timers or filesystem races. The already-open TIFF remains decodable.
            if removed {
                std::fs::remove_file(&path).unwrap();
            } else {
                std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&[0]).unwrap();
            }
            let error = read_file_with_revision(&path, "image", None, file, Ok(revision)).unwrap_err().to_string();
            assert!(error.contains("GeoTIFF:") && error.contains("Retry Add Data"), "{error}");
            assert!(
                error.contains("stable local location") && error.contains("Reload Data") && error.contains("Repair Data Source"),
                "{error}"
            );
            assert!(!error.contains("Use Reload Data, then retry the operation"), "{error}");
            assert!(error.contains(if removed { "Could not verify" } else { "file changed" }), "{error}");
            // Once stable, an ordinary Add Data read succeeds and has a verified grid revision.
            std::fs::write(&path, &bytes).unwrap();
            let loaded = read_file(&path, "image").unwrap();
            assert_eq!(loaded.bands[0].data, pixels.bands[0].data);
            loaded.check_original_file(&path).unwrap();
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn annotation_grid_capture_failure_preserves_pixels_and_reports_the_original_cause() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.tif");
        let pixels = small();
        std::fs::write(&path, write(&pixels, 0).unwrap()).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let raster = read_file_with_revision(
            &path,
            "image",
            None,
            file,
            Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "modification timestamps unavailable")),
        )
        .unwrap();
        assert_eq!((raster.width, raster.height), (pixels.width, pixels.height));
        assert_eq!(raster.bands[0].data, pixels.bands[0].data);
        let error = raster.check_original_file(&path).unwrap_err();
        assert!(error.contains("modification timestamps unavailable"), "{error}");
        assert!(error.contains("metadata support/access") && error.contains("another location"), "{error}");
        assert!(!error.contains("Reload Data"), "{error}");
        assert!(
            raster
                .clone()
                .check_original_handle(&std::fs::File::open(&path).unwrap())
                .unwrap_err()
                .contains("modification timestamps unavailable")
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn chunked_reads_count_progress_and_stop_before_or_during_serial_and_parallel_work() {
        use crate::progress::{install, tests::Counted};
        use std::io::Cursor;
        use std::sync::atomic::Ordering;

        // Sixteen strips; k=4 skips twelve of them, but progress still reaches its total.
        let values: Vec<f32> = (0..256).map(|i| i as f32).collect();
        let mut encoded = Cursor::new(Vec::new());
        {
            let mut enc = tiff::encoder::TiffEncoder::new(&mut encoded).unwrap();
            let mut img = enc.new_image::<tiff::encoder::colortype::Gray32Float>(16, 16).unwrap();
            img.rows_per_strip(1).unwrap();
            img.write_data(&values).unwrap();
        }
        let bytes = encoded.into_inner();
        for parallel in [false, true] {
            for k in [1u32, 4] {
                for after in [None, Some(0), Some(1)] {
                    let probe = after.map_or_else(|| std::sync::Arc::new(Counted::default()), Counted::cancelling_after);
                    let mut dec = Decoder::new(Cursor::new(&bytes)).unwrap();
                    let chunks = Chunks::new(&mut dec, 16, 16, 1, false);
                    let _guard = install(Some(probe.clone()));
                    let got = if parallel {
                        let caller = std::thread::current().id();
                        let open = || {
                            // Make a worker, rather than the calling thread, count the first
                            // chunk. This catches a missing thread-local probe handover even
                            // when the caller could otherwise decode the entire tiny fixture.
                            if std::thread::current().id() == caller {
                                let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
                                while probe.done.load(Ordering::Relaxed) == 0 {
                                    if std::time::Instant::now() >= end {
                                        return Err(std::io::Error::other("worker never reported chunk progress"));
                                    }
                                    std::thread::yield_now();
                                }
                            }
                            reader(Cursor::new(bytes.clone()))
                        };
                        let side = 16u32.div_ceil(k);
                        decimate_parallel(
                            chunks,
                            vec![vec![f32::NAN; (side * side) as usize]],
                            k,
                            Span::whole(side, side),
                            Uncounted::new(None, None),
                            2,
                            &open,
                            0,
                            Limits::default(),
                        )
                    } else {
                        decimate(&mut dec, 16, 16, 1, false, k, None, Uncounted::new(None, None), None)
                    };
                    if let Some(after) = after {
                        assert!(matches!(got, Err(DataError::Cancelled)), "parallel={parallel}, k={k}, after={after}: {got:?}");
                        let done = probe.done.load(Ordering::Relaxed);
                        assert!(done >= after && done < 16, "{done} chunks counted before Cancel");
                        assert!(matches!(crate::progress::check(), Err(DataError::Cancelled)), "the caller keeps its probe");
                    } else {
                        let (bands, data_type, rounded) = got.unwrap();
                        let expected: Vec<f32> = (0..16)
                            .step_by(k as usize)
                            .flat_map(|y| (0..16).step_by(k as usize).map(move |x| (y * 16 + x) as f32))
                            .collect();
                        assert_eq!(bands, vec![expected]);
                        assert_eq!(data_type, "Float32");
                        assert_eq!(rounded, vec![0]);
                        assert_eq!(probe.total.load(Ordering::Relaxed), 16);
                        assert_eq!(probe.done.load(Ordering::Relaxed), 16);
                    }
                }
            }
        }
    }

    /// The chunks of a `w × h` image in `cw × ch` tiles, `samples` to a pixel, as [`Chunks::new`]
    /// lays them out from a file's tags.
    fn chunk_grid(w: u32, h: u32, cw: u32, ch: u32, samples: usize) -> Chunks {
        let (across, down) = (w.div_ceil(cw), h.div_ceil(ch));
        Chunks { w, h, cw, ch, across, per_plane: across * down, planes: 1, samples, planar: false }
    }

    /// Every `k`-th pixel each way, whatever the chunks' shape and whether the edge ones are
    /// padded: the same pixels, in the same places; of a part of the grid, its pixels alone, from
    /// the chunks that hold it.
    #[test]
    fn decimation_keeps_the_same_pixels_whatever_the_chunk_shape() {
        let value = |x: u32, y: u32, s: usize| (y * 1000 + x * 10) as f32 + s as f32;
        for (cw, ch) in [(13, 8), (5, 3), (4, 8), (13, 1), (1, 1), (7, 5)] {
            for padded in [false, true] {
                for k in 1..=9u32 {
                    let chunks = chunk_grid(13, 8, cw, ch, 2);
                    let (gw, gh) = (13u32.div_ceil(k), 8u32.div_ceil(k));
                    for span in [
                        Span::whole(gw, gh),
                        Span { cols: (gw / 3, gw), rows: (gh / 2, gh) },
                        Span { cols: (1.min(gw - 1), gw.min(2)), rows: (0, 1) },
                    ] {
                        let (ow, oh) = (span.width(), span.height());
                        let mut bands = vec![vec![f32::NAN; ow * oh]; 2];
                        let mut read = 0;
                        for p in 0..chunks.count_in(span, k) {
                            let Some((plane, index)) = chunks.nth(p, span, k).and_then(|i| chunks.wanted(i, k, span)) else { continue };
                            read += 1;
                            let (x0, y0) = ((index % chunks.across) * cw, (index / chunks.across) * ch);
                            let (dw, rows) = if padded { (cw, ch) } else { (cw.min(13 - x0), ch.min(8 - y0)) };
                            let mut values = Vec::new();
                            for y in y0..y0 + rows {
                                for x in x0..x0 + dw {
                                    values.extend([value(x, y, 0), value(x, y, 1)]);
                                }
                            }
                            chunks.place(&mut bands, &values, None, &mut [0, 0], plane, index, k, span).unwrap();
                        }
                        for (s, band) in bands.iter().enumerate() {
                            let want: Vec<f32> = (span.rows.0..span.rows.1)
                                .flat_map(|r| (span.cols.0..span.cols.1).map(move |c| (c * k, r * k)))
                                .map(|(x, y)| value(x, y, s))
                                .collect();
                            assert_eq!(band, &want, "chunks of {cw} × {ch}, padded {padded}, every {k}th, {span:?}, band {s}");
                        }
                        // No chunk is read that holds none of the part's pixels.
                        let holding = (0..chunks.count()).filter(|i| chunks.wanted(*i, k, span).is_some()).count();
                        assert_eq!(read, holding, "chunks of {cw} × {ch}, every {k}th, {span:?}");
                    }
                }
            }
        }
    }

    /// A few hundred bytes claiming an 8-band image of 8 × 4,160,741,449 pixels in one strip (the
    /// fuzzer's `oom-76cff910…`, #1952): the strip can't be decoded, which is said before the bands
    /// of the overview made as it is read are set aside (some 2 GB of them on a computer with 16).
    #[test]
    fn a_strip_too_large_to_decode_is_refused_before_the_bands_are_set_aside() {
        let entries: [(u16, u16, u32); 9] = [
            (256, 4, 8),             // ImageWidth
            (257, 4, 4_160_741_449), // ImageLength
            (258, 3, 8),             // BitsPerSample (one value for every sample, as readers allow)
            (259, 3, 1),             // Compression: none
            (262, 3, 1),             // PhotometricInterpretation: black is zero
            (273, 4, 128),           // StripOffsets
            (277, 3, 8),             // SamplesPerPixel
            (278, 4, 4_160_741_449), // RowsPerStrip: one strip
            (279, 4, 135),           // StripByteCounts
        ];
        let mut file = b"II*\0".to_vec();
        file.extend(8u32.to_le_bytes());
        file.extend((entries.len() as u16).to_le_bytes());
        for (tag, kind, value) in entries {
            file.extend(tag.to_le_bytes());
            file.extend(kind.to_le_bytes());
            file.extend(1u32.to_le_bytes());
            file.extend(if kind == 3 { u32::from(value as u16) } else { value }.to_le_bytes());
        }
        file.extend(0u32.to_le_bytes());
        file.resize(128 + 135, 0);
        let e = read(std::io::Cursor::new(&file), "vast.tif").map(|r| (r.width, r.height)).unwrap_err().to_string();
        assert!(e.contains("a strip or tile of 8 × 4160741449 pixels is too large to read"), "{e}");
    }

    /// The first chunk's size as each decoder holds it: rows padded to whole bytes, and a tile larger
    /// than the image cropped to it for the tiff crate but not for the LZW and LERC readers.
    #[test]
    fn the_first_chunk_is_sized_as_its_decoders_hold_it() {
        // One 1-bit band, 1 × 4,160,741,449 in one strip: a byte a row, not an eighth.
        assert_eq!(chunk_grid(1, 4_160_741_449, 1, 4_160_741_449, 1).first_bytes(1), (4_160_741_449, 4_160_741_449));
        // A 1024 × 1024 byte image in one 65536-pixel tile: 1 MB cropped, 4 GB padded.
        assert_eq!(chunk_grid(1024, 1024, 65536, 65536, 1).first_bytes(8), (1 << 20, 1 << 32));
        // Bands stored apart: the tiff crate keeps a tile's full height for the later planes.
        let mut planar = chunk_grid(1024, 1024, 1024, 65536, 1);
        planar.planes = 2;
        assert_eq!(planar.first_bytes(8), (1 << 26, 1 << 26));
    }

    /// An image `u32` can just count the width (or height) of, in chunks of 2^20 pixels, every 2^20th
    /// pixel: the last chunk ends where `u32` does, so `x0 + cw` and the walk's last step pass it.
    /// In `u32` that panics in a debug build and wraps into an endless walk in a release build.
    #[test]
    fn decimating_an_image_as_wide_or_tall_as_u32_holds_places_its_last_pixel_and_ends() {
        let k = 1u32 << 20;
        for wide in [true, false] {
            let chunks = if wide { chunk_grid(u32::MAX, 1, k, 1, 1) } else { chunk_grid(1, u32::MAX, 1, k, 1) };
            assert_eq!(chunks.count(), 4096);
            let span = if wide { Span::whole(4096, 1) } else { Span::whole(1, 4096) };
            assert!((0..4096).all(|i| chunks.wanted(i, k, span) == Some((0, i as u32))));
            assert_eq!(chunks.count_in(span, k), 4096);
            // The last chunk holds one pixel fewer than the others; its first is kept.
            let mut values = vec![0.0f32; k as usize - 1];
            values[0] = 7.0;
            let mut band = vec![vec![f32::NAN; 4096]];
            chunks.place(&mut band, &values, None, &mut [0], 0, 4095, k, span).unwrap();
            assert_eq!(band[0].iter().filter(|v| !v.is_nan()).count(), 1, "wide {wide}");
            assert_eq!(band[0][4095], 7.0, "wide {wide}");
        }
    }

    /// The next multiple of the step after a chunk's corner is past `u32` there: nothing of the
    /// chunk is kept, as it isn't below the end of it, not a multiple wrapped round to 0. Where a
    /// kept pixel is the second last, its next step ends the walk.
    #[test]
    fn a_step_that_passes_u32_ends_the_chunk_instead_of_wrapping() {
        // Chunks of 3 pixels; the last holds the three pixels from 4,294,967,292.
        let chunks = chunk_grid(u32::MAX, 1, 3, 1, 1);
        let last = chunks.across - 1;
        assert_eq!(chunks.span(last), (4_294_967_292, 0, 3, 1));
        assert_eq!(chunks.wanted(u64::from(last), 1 << 20, Span::whole(4096, 1)), None);
        assert_eq!(chunks.wanted(u64::from(last), 2, Span::whole(1 << 31, 1)), Some((0, last)));
        // Every second pixel keeps 4,294,967,292 and 4,294,967,294; the output isn't made (it would
        // hold 2 GiB): the walk ending is what is checked.
        let mut bands = vec![Vec::new()];
        chunks.place(&mut bands, &[1.0, 2.0, 3.0], None, &mut [0], 0, last, 2, Span::whole(1 << 31, 1)).unwrap();
        assert!(bands[0].is_empty());
        // Chunks of no width or height, and an image of no width, are nothing, not a division by 0.
        let none = chunk_grid(0, 0, 1, 1, 1);
        assert_eq!(none.count(), 0);
        assert_eq!(none.span(0), (0, 0, 0, 0));
    }

    /// A decoded strip or tile whose values make none of its shapes is refused: a row length
    /// guessed from them would put pixels in other pixels' places, or leave them empty unsaid.
    #[test]
    fn a_chunk_of_none_of_its_shapes_is_refused_not_guessed() {
        // One 4 × 2 tile; 5 values make neither 4 × 2 nor any padded shape.
        let chunks = chunk_grid(4, 2, 4, 2, 1);
        let mut bands = vec![vec![f32::NAN; 8]];
        let e = chunks.place(&mut bands, &[1.0, 2.0, 3.0, 4.0, 5.0], None, &mut [0], 0, 0, 1, Span::whole(4, 2)).unwrap_err().to_string();
        assert!(e.contains("decodes to 5 values, not the 4 × 2 pixels"), "{e}");
        assert!(bands[0].iter().all(|v| v.is_nan()), "nothing placed from a refused chunk");
        // All 8 are placed where they belong.
        let all: Vec<f32> = (0..8).map(|v| v as f32).collect();
        chunks.place(&mut bands, &all, None, &mut [0], 0, 0, 1, Span::whole(4, 2)).unwrap();
        assert_eq!(bands[0], all);
    }

    /// Who holds the rights (the Copyright tag) is written by both writers and read back, as the
    /// ASCII a TIFF tag holds (`©` as `(c)`, control characters as spaces, others as `?`); a
    /// raster without it writes none.
    #[test]
    fn copyright_round_trips() {
        let mut r = small();
        assert_eq!(read(std::io::Cursor::new(write(&r, 0).unwrap()), "none").unwrap().copyright, None);
        r.copyright = Some("© OpenStreetMap contributors | Bundesamt für Landestopografie\0\n".into());
        let want = Some("(c) OpenStreetMap contributors | Bundesamt f?r Landestopografie".to_string());
        // One written as ?; the NUL and line break at the end are trimmed, not counted.
        assert_eq!(super::super::tag_text_changes(r.copyright.as_deref().unwrap()), (1, 1, 0));
        // Inside, a line break and U+0085 (a control character outside ASCII) are spaces.
        assert_eq!(super::super::tag_text_changes("a\nb\u{85}c ü"), (0, 1, 2));
        assert_eq!(
            super::super::tag_text_note("© a\nb ü").as_deref(),
            Some(
                "the Copyright tag holds ASCII only, so it is written with 1 © or ® as (c) or (R), 1 character as ?, 1 control character as a space"
            )
        );
        assert_eq!(super::super::tag_text_note("plain"), None);
        assert_eq!(super::super::tag_text_note(""), None);
        // Nothing a tag can hold: no tag is written, and that is said.
        assert_eq!(
            super::super::tag_text_note("\0\n").as_deref(),
            Some("the credits hold nothing a Copyright tag can, so the file has none")
        );
        let mut blank = small();
        blank.copyright = Some("\0\n".into());
        assert_eq!(read(std::io::Cursor::new(write(&blank, 0).unwrap()), "none").unwrap().copyright, None);
        // A Copyright tag that isn't text (bytes, not ASCII) is left out, and said.
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut enc = tiff::encoder::TiffEncoder::new(&mut buf).unwrap();
            let mut img = enc.new_image::<tiff::encoder::colortype::Gray8>(2, 2).unwrap();
            img.encoder().write_tag(Tag::Copyright, &[0xA9u8, 0x20, 0x58][..]).unwrap();
            img.write_data(&[1, 2, 3, 4]).unwrap();
        }
        let odd = read(std::io::Cursor::new(buf.into_inner()), "odd").unwrap();
        assert_eq!(odd.copyright, None);
        assert!(odd.lost.iter().any(|l| l.starts_with("its Copyright tag: it isn't ASCII text")), "{:?}", odd.lost);
        assert_eq!(super::super::tag_text("a\nb\u{85}c ü"), "a b c ?");
        // A scan (a TIFF with no GeoTIFF tags) keeps it too.
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut enc = tiff::encoder::TiffEncoder::new(&mut buf).unwrap();
            let mut img = enc.new_image::<tiff::encoder::colortype::Gray8>(2, 2).unwrap();
            img.encoder().write_tag(Tag::Copyright, "(c) Scan").unwrap();
            img.write_data(&[1, 2, 3, 4]).unwrap();
        }
        let scan = read(std::io::Cursor::new(buf.into_inner()), "scan").unwrap();
        assert_eq!((scan.georeferenced, scan.copyright.as_deref()), (false, Some("(c) Scan")));
        assert_eq!(read(std::io::Cursor::new(write(&r, 0).unwrap()), "tif").unwrap().copyright, want);
        let cog = super::super::cogwrite::write(&r, 0).unwrap();
        assert_eq!(read(std::io::Cursor::new(cog), "cog").unwrap().copyright, want);
    }

    /// A user-defined system's keys are written with the raster type as area, as the corner
    /// tiepoint is: one first read from a point file isn't placed half a pixel off.
    #[test]
    fn custom_keys_are_written_as_area() {
        let point = [1u16, 1, 0, 3, 1024, 0, 1, 1, 1025, 0, 1, 2, 3072, 0, 1, 32767];
        assert_eq!(as_area(&point), [1, 1, 0, 3, 1024, 0, 1, 1, 1025, 0, 1, 1, 3072, 0, 1, 32767]);
        // A key whose value is elsewhere (not in place) is left alone, as is a short directory.
        assert_eq!(as_area(&[1, 1, 0, 1, 1025, 34737, 1, 2]), [1, 1, 0, 1, 1025, 34737, 1, 2]);
        assert_eq!(as_area(&[1, 1]), [1, 1]);
    }

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

    /// A grey 8-bit band is written as bytes, its no-data cells as the no-data value; without a
    /// no-data value to mark an empty cell, or with a fraction, it stays floats.
    #[test]
    fn grey_bytes_round_trip_as_bytes() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 3.0, pixel_width: 1.0, pixel_height: -1.0 };
        let byte = |cells: Vec<f32>, nodata| {
            let r = Raster::new("g", 3, 1, vec![cells], gt, Crs::LonLat, nodata, ColorHint::Gray, "UInt8").unwrap();
            read(std::io::Cursor::new(write(&r, 0).unwrap()), "back").unwrap()
        };
        let back = byte(vec![1.0, 0.0, 255.0], Some(0.0));
        assert_eq!((back.data_type.as_str(), back.nodata), ("UInt8", Some(0.0)));
        assert_eq!((back.value(0, 0, 0), back.value(0, 1, 0), back.value(0, 2, 0)), (Some(1.0), None, Some(255.0)));
        assert_eq!(byte(vec![1.0, f32::NAN, 2.0], None).data_type, "Float32");
        assert_eq!(byte(vec![1.0, 2.5, 2.0], Some(0.0)).data_type, "Float32");
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

    /// A 2 × 2 byte image whose upper left corner is `corner`, `step` a pixel, with this GeoKey
    /// directory (none: the file gives no coordinate system).
    fn placed_tiff(corner: (f64, f64), step: f64, keys: Option<&[u16]>) -> Vec<u8> {
        use tiff::encoder::{TiffEncoder, colortype};
        let mut out = std::io::Cursor::new(Vec::new());
        let mut enc = TiffEncoder::new(&mut out).unwrap_or_else(|e| panic!("{e}"));
        let mut img = enc.new_image::<colortype::Gray8>(2, 2).unwrap_or_else(|e| panic!("{e}"));
        img.encoder().write_tag(Tag::ModelPixelScaleTag, &[step, step, 0.0][..]).unwrap_or_else(|e| panic!("{e}"));
        img.encoder().write_tag(Tag::ModelTiepointTag, &[0.0f64, 0.0, 0.0, corner.0, corner.1, 0.0][..]).unwrap_or_else(|e| panic!("{e}"));
        if let Some(keys) = keys {
            img.encoder().write_tag(Tag::GeoKeyDirectoryTag, keys).unwrap_or_else(|e| panic!("{e}"));
        }
        img.write_data(&[1u8, 2, 3, 4]).unwrap_or_else(|e| panic!("{e}"));
        out.into_inner()
    }

    /// A file asked for in a system (a WCS coverage, #2032) that doesn't say its own is read in it
    /// when its coordinates aren't degrees, and says so; one that names its system, or whose
    /// coordinates are degrees, is read as [`read`] reads it.
    #[test]
    fn a_file_without_its_system_is_read_in_the_one_asked_for() {
        let lv95 = Crs::from_epsg(2056).unwrap();
        let metres = placed_tiff((2_600_000.0, 1_200_100.0), 10.0, None);
        assert!(read(std::io::Cursor::new(metres.clone()), "g").is_err());
        let r = read_asked_in(std::io::Cursor::new(metres), "g", lv95).unwrap();
        assert_eq!((r.crs, r.transform.origin_x), (lv95, 2_600_000.0));
        assert!(r.crs_assumed && r.lost.iter().any(|l| l.contains("read in EPSG:2056, the system it was asked for in")), "{:?}", r.lost);
        let named = placed_tiff((600_000.0, 5_200_000.0), 10.0, Some(&[1, 1, 0, 1, 3072, 0, 1, 25832]));
        let r = read_asked_in(std::io::Cursor::new(named), "g", lv95).unwrap();
        assert_eq!((r.crs, r.crs_assumed), (Crs::from_epsg(25832).unwrap(), false));
        let degrees = placed_tiff((7.4, 47.0), 0.01, None);
        assert_eq!(read_asked_in(std::io::Cursor::new(degrees), "g", lv95).unwrap().crs, Crs::LonLat);
    }

    /// A grid whose file gives no usable coordinate system but whose coordinates fit in degrees is
    /// read as WGS 84 and says why, once. A file that gives its system says nothing of the kind, and
    /// metres of a projection that can't be read are still refused with the reason (AGENTS.md, Never
    /// silent).
    #[test]
    fn a_grid_read_as_wgs84_says_why() {
        let read_tiff = |bytes: Vec<u8>| read(std::io::Cursor::new(bytes), "g");
        // No GeoKeys at all.
        let r = read_tiff(placed_tiff((10.0, 50.0), 0.5, None)).unwrap();
        assert_eq!(r.crs, Crs::LonLat);
        assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
        assert!(r.lost[0].contains("doesn't say its coordinate system") && r.lost[0].contains("WGS 84"), "{:?}", r.lost);
        // Its own projection (a coordinate transformation that isn't supported) can't be read, but its
        // coordinates fit in degrees: the reason is said, not dropped.
        let own = [1u16, 1, 0, 3, 1024, 0, 1, 1, 3072, 0, 1, 32767, 3075, 0, 1, 99];
        let r = read_tiff(placed_tiff((10.0, 50.0), 0.5, Some(&own))).unwrap();
        assert_eq!(r.crs, Crs::LonLat);
        assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
        assert!(
            r.lost[0].starts_with("its own coordinate system can't be read (")
                && r.lost[0].contains("coordinate transformation 99")
                && r.lost[0].ends_with("so it's read as WGS 84"),
            "{:?}",
            r.lost
        );
        // Metres of the same projection are refused, saying why.
        let e = read_tiff(placed_tiff((500_000.0, 4_000_000.0), 30.0, Some(&own))).unwrap_err().to_string();
        assert!(e.contains("coordinate transformation 99"), "{e}");
        // A geographic system with no EPSG code: its datum isn't read.
        let user = [1u16, 1, 0, 2, 1024, 0, 1, 2, 2048, 0, 1, 32767];
        let r = read_tiff(placed_tiff((10.0, 50.0), 0.5, Some(&user))).unwrap();
        assert_eq!(r.crs, Crs::LonLat);
        assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
        assert!(r.lost[0].contains("no EPSG code, so its datum isn't read"), "{:?}", r.lost);
        // A user-defined model type (32767) whose keys don't define a system but name a code is
        // read by the code, as before.
        let named = [1u16, 1, 0, 3, 1024, 0, 1, 32767, 1025, 0, 1, 1, 3072, 0, 1, 32633];
        let r = read_tiff(placed_tiff((500_000.0, 4_000_000.0), 30.0, Some(&named))).unwrap();
        assert_eq!(r.crs, Crs::from_epsg(32633).unwrap());
        // Files that give their system say nothing: WGS 84 by its code, and a UTM zone.
        let wgs84 = [1u16, 1, 0, 3, 1024, 0, 1, 2, 1025, 0, 1, 1, 2048, 0, 1, 4326];
        let utm = [1u16, 1, 0, 3, 1024, 0, 1, 1, 1025, 0, 1, 1, 3072, 0, 1, 32633];
        for (bytes, crs) in [
            (placed_tiff((10.0, 50.0), 0.5, Some(&wgs84)), Crs::LonLat),
            (placed_tiff((500_000.0, 4_000_000.0), 30.0, Some(&utm)), Crs::from_epsg(32633).unwrap()),
        ] {
            let r = read_tiff(bytes).unwrap();
            assert_eq!(r.crs, crs);
            assert!(r.lost.is_empty(), "{:?}", r.lost);
        }
    }

    /// A rotated or skewed image says it was resampled onto a north-up grid, once, and how; its
    /// pixels are said to be coarser than the file's only when the grid had to be. A north-up image
    /// says nothing of the kind.
    #[test]
    fn a_turned_image_says_how_it_was_resampled() {
        let rotated = [30.0, 5.0, 0.0, 400_000.0, 3.0, -30.0, 0.0, 5_000_000.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let r = read(std::io::Cursor::new(tests_support::matrix_tiff(rotated, false)), "rot").unwrap();
        assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
        let said = &r.lost[0];
        assert!(said.starts_with("it is rotated or skewed, so it is resampled (nearest pixel) onto a north-up grid of "), "{said}");
        assert!(said.contains(&format!("{} × {} pixels of 30.25 each", r.width, r.height)), "{said}");
        assert!(!said.contains("coarser"), "{said}");
        // Past the sample limit the grid is coarser than the file's pixels, and says so.
        let file = tests_support::matrix_tiff(rotated, false);
        let r = read_limited(reader(std::io::Cursor::new(&file)).unwrap(), "coarse", 200, None).unwrap();
        assert!(r.width * r.height <= 200 && r.transform.pixel_width > 30.25);
        assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
        assert!(r.lost[0].contains("coarser") && r.lost[0].contains("30.25"), "{:?}", r.lost);
        // Decimation makes the source read coarser before north_up; compare to the original file.
        let r = read_limited(reader(std::io::Cursor::new(&file)).unwrap(), "decimated", 50, None).unwrap();
        assert_eq!(r.overview, Some((20, 10)));
        assert!(r.width * r.height <= 50);
        assert!(r.lost.iter().any(|n| n.contains("coarser than the file's (30.25)")), "{:?}", r.lost);
        // A stored overview also uses the full-resolution affine for the comparison.
        let mut out = std::io::Cursor::new(Vec::new());
        {
            use tiff::encoder::{TiffEncoder, colortype};
            let mut enc = TiffEncoder::new(&mut out).unwrap();
            let mut image = enc.new_image::<colortype::Gray8>(20, 10).unwrap();
            image.encoder().write_tag(Tag::Unknown(34264), &rotated[..]).unwrap();
            let keys = [1u16, 1, 0, 1, 3072, 0, 1, 32633];
            image.encoder().write_tag(Tag::GeoKeyDirectoryTag, &keys[..]).unwrap();
            image.write_data(&[100u8; 200]).unwrap();
            let mut image = enc.new_image::<colortype::Gray8>(10, 5).unwrap();
            image.encoder().write_tag(Tag::NewSubfileType, 1u32).unwrap();
            image.write_data(&[100u8; 50]).unwrap();
        }
        let r = read_limited(reader(std::io::Cursor::new(out.into_inner())).unwrap(), "overview", 50, None).unwrap();
        assert_eq!(r.overview, Some((20, 10)));
        assert!(r.lost.iter().any(|n| n.contains("coarser than the file's (30.25)")), "{:?}", r.lost);
        // The same image placed north up needs no resampling, and says nothing.
        let level = [30.0, 0.0, 0.0, 400_000.0, 0.0, -30.0, 0.0, 5_000_000.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let r = read(std::io::Cursor::new(tests_support::matrix_tiff(level, false)), "level").unwrap();
        assert!(r.lost.is_empty(), "{:?}", r.lost);
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

    /// An RGBA export declares its fourth sample alpha, so it reads back as alpha, not as RGB and a
    /// data band (bytes and floats alike).
    #[test]
    fn rgba_export_reads_back_with_alpha() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 2.0, pixel_width: 1.0, pixel_height: -1.0 };
        for (dt, factor) in [("UInt8", 1.0), ("Float32", 0.01)] {
            let bands: Vec<Vec<f32>> = [200.0, 100.0, 50.0, 255.0].iter().map(|v| vec![v * factor; 4]).collect();
            let r = Raster::new("rgba", 2, 2, bands, gt, Crs::LonLat, None, ColorHint::Rgb { alpha: true }, dt).unwrap();
            let back = read(std::io::Cursor::new(write(&r, 0).unwrap()), "rgba").unwrap();
            assert_eq!((back.band_count(), back.color.clone()), (4, ColorHint::Rgb { alpha: true }), "{dt}");
        }
    }

    /// An 8-bit raster with no no-data value keeps a NaN cell as a NaN, in floats, never as 0: grey
    /// with a second band (`write_all`), RGB without alpha, and RGBA with a NaN colour under an
    /// opaque alpha (`write` and `write_all`). A NaN colour the alpha hides, and a raster with no
    /// NaN, stay bytes. An RGBA raster written as floats keeps its alpha band and colour hint.
    #[test]
    fn nan_cells_of_byte_rasters_without_no_data_are_not_written_as_zero() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
        let nan = f32::NAN;
        let raster = |bands: Vec<Vec<f32>>, color| Raster::new("nan", 3, 1, bands, gt, Crs::LonLat, None, color, "UInt8").unwrap();
        let rgb = |red: [f32; 3], blue: [f32; 3]| vec![red.to_vec(), vec![5.0; 3], blue.to_vec()];
        let rgba = |red: [f32; 3], blue: [f32; 3], alpha: [f32; 3]| {
            let mut bands = rgb(red, blue);
            bands.push(alpha.to_vec());
            raster(bands, ColorHint::Rgb { alpha: true })
        };
        let cases = [
            ("two bands, NaN", raster(vec![vec![1.0, nan, 2.0], vec![3.0; 3]], ColorHint::Gray), true),
            ("two bands, no NaN", raster(vec![vec![1.0, 4.0, 2.0], vec![3.0; 3]], ColorHint::Gray), false),
            ("RGB, NaN", raster(rgb([1.0, nan, 2.0], [6.0; 3]), ColorHint::Rgb { alpha: false }), true),
            ("RGB, no NaN", raster(rgb([1.0, 4.0, 2.0], [6.0; 3]), ColorHint::Rgb { alpha: false }), false),
            ("RGBA, NaN under an opaque alpha", rgba([1.0, nan, 2.0], [6.0, 6.0, nan], [255.0; 3]), true),
            ("RGBA, NaN under a clear alpha", rgba([1.0, nan, 2.0], [6.0, nan, 6.0], [255.0, 0.0, 255.0]), false),
            ("RGBA, no NaN", rgba([1.0, 4.0, 2.0], [6.0; 3], [255.0; 3]), false),
        ];
        for (case, r, floats) in &cases {
            for (writer, bytes) in [("write", write(r, 0).unwrap()), ("write_all", write_all(r).unwrap())] {
                // `write` keeps one band of a two-band grey raster: only `write_all` is asked of it.
                if writer == "write" && r.bands.len() == 2 {
                    continue;
                }
                let back = read(std::io::Cursor::new(bytes), "back").unwrap();
                let at = format!("{case}, {writer}");
                assert_eq!(back.data_type, if *floats { "Float32" } else { "UInt8" }, "{at}");
                assert_eq!((back.band_count(), &back.color), (r.band_count(), &r.color), "{at}");
                if *floats {
                    // An alpha band is on the float scale: `rgba_written_as_floats_keeps_its_opacity`.
                    let alpha = if r.color == (ColorHint::Rgb { alpha: true }) { Some(3) } else { None };
                    for (b, band) in r.bands.iter().enumerate().filter(|(b, _)| Some(*b) != alpha) {
                        let same = |(a, b): (&f32, &f32)| a == b || (a.is_nan() && b.is_nan());
                        assert!(band.data.iter().zip(&back.bands[b].data).all(same), "{at}, band {b}: {:?}", back.bands[b].data);
                    }
                }
            }
        }
    }

    /// An RGBA raster written as floats keeps how see-through each pixel is: its alpha is on the
    /// float scale (1 is opaque) once read back, whatever scale the integer raster had. The
    /// colours stay as they are (a float raster is stretched by its own range when drawn).
    #[test]
    fn rgba_written_as_floats_keeps_its_opacity() {
        use super::super::cogwrite;
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
        let rgba = |data_type: &str, red: [f32; 3], alpha: [f32; 3]| {
            let bands = vec![red.to_vec(), vec![5.0; 3], vec![6.0; 3], alpha.to_vec()];
            Raster::new("rgba", 3, 1, bands, gt, Crs::LonLat, None, ColorHint::Rgb { alpha: true }, data_type).unwrap()
        };
        // The opacity (0-255) the map draws a pixel with: its alpha over the type's opaque value.
        let opacity =
            |r: &Raster, i: usize| ((r.bands[3].data[i] / crate::raster::alpha_max(&r.data_type)).clamp(0.0, 1.0) * 255.0).round();
        let nan = f32::NAN;
        let cases = [
            ("UInt8, NaN colour", rgba("UInt8", [1.0, nan, 2.0], [255.0, 128.0, 255.0])),
            ("UInt16", rgba("UInt16", [1000.0, 2000.0, 3000.0], [65535.0, 32768.0, 65535.0])),
            ("Int16", rgba("Int16", [100.0, -200.0, 300.0], [32767.0, 16384.0, 32767.0])),
            ("Float32", rgba("Float32", [0.1, 0.2, 0.3], [1.0, 0.5, 1.0])),
            ("UInt8, five bands, NaN colour", {
                let mut r = rgba("UInt8", [1.0, nan, 2.0], [255.0, 128.0, 255.0]);
                r.bands.push(r.bands[1].clone());
                r
            }),
        ];
        for (case, r) in &cases {
            let mut files = vec![("write", write(r, 0).unwrap()), ("write_all", write_all(r).unwrap())];
            // The COG writer makes a cell with a NaN colour fully see-through, by design.
            if !case.contains("NaN") {
                files.push(("cog", cogwrite::write(r, 0).unwrap()));
            }
            for (writer, bytes) in files {
                let back = read(std::io::Cursor::new(bytes), "back").unwrap();
                let at = format!("{case}, {writer}");
                // `write` keeps the colour bands and alpha; `write_all` every band.
                let kept = if writer == "write" { 4 } else { r.band_count() };
                assert_eq!((back.band_count(), &back.color), (kept, &ColorHint::Rgb { alpha: true }), "{at}");
                for i in 0..3 {
                    assert_eq!(opacity(&back, i), opacity(r, i), "{at}, pixel {i}: alpha {:?}", back.bands[3].data);
                }
            }
        }
        // Grey with alpha, past what `write` keeps (`write_all`): the same.
        let grey = Raster::new(
            "grey",
            3,
            1,
            vec![vec![1.0, nan, 2.0], vec![255.0, 128.0, 255.0]],
            gt,
            Crs::LonLat,
            None,
            ColorHint::GrayAlpha,
            "UInt8",
        )
        .unwrap();
        let back = read(std::io::Cursor::new(write_all(&grey).unwrap()), "back").unwrap();
        let opaque = crate::raster::alpha_max(&back.data_type);
        assert_eq!((back.band_count(), &back.color), (2, &ColorHint::GrayAlpha));
        assert_eq!(back.bands[1].data.iter().map(|a| (a / opaque * 255.0).round()).collect::<Vec<_>>(), [255.0, 128.0, 255.0]);
    }

    /// An alpha cell equal to the no-data value is no value, and stays one when written as floats:
    /// it isn't scaled to 1 (opaque); the cells beside it are. (The COG writer has no no-data tag
    /// beside an alpha band: its alpha says which pixels have no value.)
    #[test]
    fn alpha_equal_to_no_data_stays_no_data_when_written_as_floats() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
        let rgba = |extra: usize| {
            let mut bands = vec![vec![100.0; 3], vec![200.0; 3], vec![300.0; 3], vec![32767.0, 16384.0, 32767.0]];
            bands.extend(vec![vec![5.0; 3]; extra]);
            Raster::new("rgba", 3, 1, bands, gt, Crs::LonLat, Some(32767.0), ColorHint::Rgb { alpha: true }, "Int16").unwrap()
        };
        for (case, r) in [("four bands", rgba(0)), ("five bands", rgba(1))] {
            let mut files = vec![("write_all", write_all(&r).unwrap())];
            if r.band_count() == 4 {
                files.push(("write", write(&r, 0).unwrap()));
            }
            for (writer, bytes) in files {
                let back = read(std::io::Cursor::new(bytes), "back").unwrap();
                let at = format!("{case}, {writer}");
                assert_eq!(back.value(3, 0, 0), None, "{at}: {:?}", back.bands[3].data);
                assert_eq!(back.value(3, 2, 0), None, "{at}");
                assert!(back.value(3, 1, 0).is_some_and(|a| (a - 0.5).abs() < 1e-3), "{at}: {:?}", back.bands[3].data);
            }
        }
    }

    /// A float file's alpha is scaled to 0–1, so an opaque alpha (1) must not become the no-data
    /// value the file's tag names (here 1): a valid cell stays valid and an invalid one stays
    /// invalid, in every band, whatever the tag says. The COG writer has no tag beside an alpha
    /// band, so its opaque alpha can't hit one.
    #[test]
    fn scaled_alpha_does_not_become_the_no_data_value() {
        use super::super::cogwrite;
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
        let rgba = |extra: usize| {
            // Pixel 0: valid, opaque. 1: valid, half see-through. 2: no red (1 is no-data), opaque.
            // 3: opaque colours, an alpha that is the no-data value.
            let mut bands = vec![vec![100.0, 200.0, 1.0, 100.0], vec![100.0; 4], vec![300.0; 4], vec![32767.0, 16384.0, 32767.0, 1.0]];
            bands.extend(vec![vec![5.0; 4]; extra]);
            Raster::new("rgba", 4, 1, bands, gt, Crs::LonLat, Some(1.0), ColorHint::Rgb { alpha: true }, "Int16").unwrap()
        };
        for (case, r) in [("four bands", rgba(0)), ("five bands", rgba(1))] {
            let mut files = vec![("write_all", write_all(&r).unwrap())];
            if r.band_count() == 4 {
                files.push(("write", write(&r, 0).unwrap()));
            }
            for (writer, bytes) in files {
                let back = read(std::io::Cursor::new(bytes), "back").unwrap();
                let at = format!("{case}, {writer}");
                let alpha = |i| back.value(3, i, 0);
                assert!(alpha(0).is_some_and(|a| (a - 1.0).abs() < 1e-6), "{at}: {:?}", back.bands[3].data);
                assert!(alpha(1).is_some_and(|a| (a - 0.5).abs() < 1e-3), "{at}");
                assert!(alpha(2).is_some_and(|a| (a - 1.0).abs() < 1e-6), "{at}");
                assert_eq!(alpha(3), None, "{at}");
                assert_eq!((back.value(0, 0, 0), back.value(0, 2, 0)), (Some(100.0), None), "{at}");
                assert_eq!(back.value(2, 0, 0), Some(300.0), "{at}");
            }
        }
        let back = read(std::io::Cursor::new(cogwrite::write(&rgba(0), 0).unwrap()), "cog").unwrap();
        assert_eq!(back.nodata, None);
        assert!(back.value(3, 0, 0).is_some_and(|a| (a - 1.0).abs() < 1e-6) && back.value(3, 2, 0).is_some());
    }

    #[test]
    fn rgbi_byte_export_does_not_turn_nir_into_alpha() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 2.0, pixel_width: 1.0, pixel_height: -1.0 };
        let r = Raster::new(
            "rgbi",
            2,
            2,
            vec![vec![200.0; 4], vec![100.0; 4], vec![50.0; 4], vec![0.0; 4]],
            gt,
            Crs::LonLat,
            None,
            ColorHint::Rgb { alpha: false },
            "UInt8",
        )
        .unwrap();
        assert_eq!(written_bands(&r, 0), vec![0, 1, 2, 3]);
        let back = read(std::io::Cursor::new(write(&r, 0).unwrap()), "rgb").unwrap();
        assert_eq!((back.band_count(), back.color.clone()), (4, ColorHint::Rgb { alpha: false }));
        assert_eq!(back.value(0, 0, 0), Some(200.0));
        assert_eq!(back.value(3, 0, 0), Some(0.0));
    }

    /// `write_all` keeps the bands `write` leaves out: every band of a grey raster, grey and its
    /// alpha (with a data band after it too), and a colour raster's bands past RGB and alpha, each
    /// read back as written, with its colour; 8-bit bands stay 8-bit.
    #[test]
    fn write_all_keeps_every_band() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 2.0, pixel_width: 1.0, pixel_height: -1.0 };
        let raster = |n: usize, color: ColorHint| {
            let bands = (0..n).map(|b| (0..6).map(|i| (b * 10 + i) as f32 + 0.5).collect()).collect();
            Raster::new("r", 3, 2, bands, gt, Crs::LonLat, None, color, "Float32").unwrap()
        };
        for r in [
            raster(3, ColorHint::Gray),
            raster(2, ColorHint::GrayAlpha),
            raster(3, ColorHint::GrayAlpha),
            raster(6, ColorHint::Rgb { alpha: true }),
        ] {
            assert!(written_bands(&r, 0).len() < r.band_count());
            let back = read(std::io::Cursor::new(write_all(&r).unwrap()), "r").unwrap();
            assert_eq!((back.band_count(), &back.color), (r.band_count(), &r.color));
            assert!((0..r.band_count()).all(|b| back.bands[b].data == r.bands[b].data), "{:?}", r.color);
        }
        // Two bands hinted as colour with alpha are too few for colour: neither is marked alpha.
        let two = raster(2, ColorHint::Rgb { alpha: true });
        let back = read(std::io::Cursor::new(write_all(&two).unwrap()), "r").unwrap();
        assert_eq!((back.band_count(), &back.color), (2, &ColorHint::Gray));
        // An 8-bit colour image with more bands stays 8-bit, so it reads back with the same range.
        let bytes = (0..5).map(|b| (0..6).map(|i| (b * 40 + i) as f32).collect()).collect();
        let rgbi = Raster::new("b", 3, 2, bytes, gt, Crs::LonLat, None, ColorHint::Rgb { alpha: false }, "UInt8").unwrap();
        let back = read(std::io::Cursor::new(write_all(&rgbi).unwrap()), "b").unwrap();
        assert_eq!((back.band_count(), &back.color, back.data_type.as_str()), (5, &rgbi.color, "UInt8"));
        assert!((0..5).all(|b| back.bands[b].data == rgbi.bands[b].data));
        // A float raster holding byte-range whole values stays float, with its own range.
        let floats = vec![vec![0.0, 1.0, 7.0, 9.0, 4.0, 3.0], vec![1.0; 6]];
        let floats = Raster::unmeasured("f", 3, 2, floats, gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        assert_eq!(read(std::io::Cursor::new(write_all(&floats).unwrap()), "f").unwrap().data_type, "Float32");
        // An 8-bit raster holding a fraction (a resampled mean) keeps it: written as floats.
        let mean = vec![vec![0.0, 1.5, 7.0, 9.0, 0.0, 3.0], vec![1.0; 6]];
        let means = Raster::new("m", 3, 2, mean, gt, Crs::LonLat, Some(0.0), ColorHint::Gray, "UInt8").unwrap();
        let back = read(std::io::Cursor::new(write_all(&means).unwrap()), "m").unwrap();
        assert_eq!(back.value(0, 1, 0), Some(1.5));
        // A raster without statistics (read in part) is judged by its values: 1200 isn't a byte.
        let tall = vec![vec![1200.0, 0.0, 300.0, 1.0, 2.0, 3.0], vec![1.0; 6]];
        let tall = Raster::unmeasured("t", 3, 2, tall, gt, Crs::LonLat, Some(0.0), ColorHint::Gray, "UInt8").unwrap();
        let back = read(std::io::Cursor::new(write_all(&tall).unwrap()), "t").unwrap();
        assert_eq!((back.value(0, 0, 0), back.value(0, 2, 0)), (Some(1200.0), Some(300.0)));
        // What `write` keeps whole is written as `write` writes it.
        let rgb = raster(3, ColorHint::Rgb { alpha: false });
        assert_eq!(write_all(&rgb).unwrap(), write(&rgb, 0).unwrap());
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
        assert!(p.contains("+proj=lcc") && p.contains("+lon_0=-97.5"), "{p}");
        // The same place either way.
        let c = rustgis_geom::Coord { x: 0.0, y: 0.0 };
        let (a, b) = (r.crs.transformer().unwrap().to_lonlat(c).unwrap(), back.crs.transformer().unwrap().to_lonlat(c).unwrap());
        assert!((a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9 && (a.x + 97.5).abs() < 1e-6, "{a:?} {b:?}");
        // What GeoTIFF's projection keys can't say (another method, a unit, a shift, a prime
        // meridian) goes in an ESRI PE citation, as GDAL 3.12.1 writes it, and reads back alike.
        assert!(terrio_keys(&CustomCrs { proj4: "junk".into(), ..CustomCrs::default() }).is_none());
        for def in [
            "+proj=moll +lon_0=0 +datum=WGS84",
            "+proj=tmerc +lon_0=0 +units=us-ft +datum=WGS84",
            "+proj=stere +lat_0=45 +lat_ts=30 +datum=WGS84",
            "+proj=lcc +lat_1=30 +lat_0=40 +lon_0=-96 +datum=WGS84",
            "+proj=eqc +lat_0=30 +lon_0=0 +datum=WGS84",
            "+proj=laea +lat_0=52 +lon_0=10 +datum=WGS84",
            "+proj=merc +lat_ts=33 +datum=WGS84",
            "+proj=tmerc +ellps=bessel +towgs84=598.1,73.7,418.2",
            "+proj=lcc +lat_1=40 +lat_0=40 +pm=paris +a=6378249.2 +b=6356515",
            // A one-parallel conic with a scale factor, and a polar stereographic.
            "+proj=lcc +lat_1=46.8 +lat_2=46.8 +lat_0=46.8 +lon_0=2.3 +k_0=0.99987742 +datum=WGS84",
            "+proj=stere +lat_0=90 +lat_ts=70 +lon_0=-45 +k=1 +x_0=0 +y_0=0 +datum=WGS84 +units=m",
        ] {
            keys_round_trip(def);
        }
    }

    /// User-defined keys GDAL reads otherwise than the plain key set: the centre's easting and
    /// northing (ProjCenterEastingGeoKey, ProjCenterNorthingGeoKey) in place of the false ones,
    /// and a Hotine without its rectified grid angle, which GDAL takes as 90°. Where GDAL 3.12.2
    /// places them (its WKT2, then pyproj 3.8.0 from EPSG:4326; 3.10.3 agrees). RustGIS read the
    /// first and last as GDAL does and refused the second.
    #[test]
    fn projection_keys_are_read_as_gdal_reads_them() {
        fn keys(transformation: u16, params: &[(u16, f64)]) -> (Vec<u16>, Vec<f64>) {
            // Projected, user-defined, on WGS 84, in metres.
            let mut entries: Vec<[u16; 4]> = vec![
                [1024, 0, 1, 1],
                [1025, 0, 1, 1],
                [2048, 0, 1, 4326],
                [3072, 0, 1, 32767],
                [3074, 0, 1, 32767],
                [3075, 0, 1, transformation],
                [3076, 0, 1, 9001],
            ];
            let mut doubles = vec![];
            for (k, v) in params {
                entries.push([*k, 34736, 1, doubles.len() as u16]);
                doubles.push(*v);
            }
            entries.sort_unstable();
            let mut keys = vec![1, 1, 0, entries.len() as u16];
            keys.extend(entries.into_iter().flatten());
            (keys, doubles)
        }
        let cases = [
            (
                keys(1, &[(3080, 9.0), (3081, 0.0), (3092, 0.9996), (3090, 500_000.0), (3091, 0.0)]),
                (10.0, 45.0),
                (578_815.303, 4_983_436.768),
            ),
            (
                keys(3, &[(3088, 10.0), (3089, 40.0), (3094, 30.0), (3093, 1.0), (3082, 0.0), (3083, 0.0)]),
                (11.0, 41.0),
                (5_038_140.229, -17_104.251),
            ),
            // Polar stereographic about the pole with a standard parallel beside it: GDAL 3.12.2
            // (and 3.10.3) take the pole as the standard parallel, scale 1, as RustGIS read it.
            (keys(15, &[(3095, 0.0), (3081, 90.0), (3078, 71.0), (3082, 0.0), (3083, 0.0)]), (10.0, 80.0), (194_428.508, -1_102_658.862)),
        ];
        for ((k, d), (lon, lat), (x, y)) in cases {
            let crs = custom_crs(&k, &d, "").unwrap_or_else(|e| panic!("{e}: {k:?}"));
            let p = crs.transformer().unwrap().from_lonlat(rustgis_geom::Coord::new(lon, lat)).unwrap();
            assert!((p.x - x).abs() < 0.01 && (p.y - y).abs() < 0.01, "{p:?}: {k:?}");
        }
    }

    /// `def` written as GeoKeys ([`terrio_keys`]) and read back ([`custom_crs`]): a system that
    /// places points as `def` does, near its centre. The keys and the system read back.
    fn keys_round_trip(def: &str) -> (Vec<u16>, Vec<f64>, String, Crs) {
        let crs = Crs::parse(def).unwrap_or_else(|e| panic!("{def}: {e}"));
        let d = crs.custom_def().unwrap();
        let (k, dd, a) = terrio_keys(&d).unwrap_or_else(|| panic!("{def}"));
        let back = custom_crs(&k, &dd, &a).unwrap_or_else(|e| panic!("{def}: {e}"));
        let num = |key: &str| def.split_whitespace().find_map(|t| t.strip_prefix(key)?.parse::<f64>().ok());
        let (lon0, lat0) = (num("+lon_0=").unwrap_or(0.0), num("+lat_0=").unwrap_or(45.0));
        let at = rustgis_geom::Coord::new(lon0 + 1.0, if lat0.abs() >= 80.0 { lat0.signum() * 80.0 } else { lat0 + 1.0 });
        let (p, q) = (crs.transformer().unwrap().from_lonlat(at).unwrap(), back.transformer().unwrap().from_lonlat(at).unwrap());
        assert!((p.x - q.x).hypot(p.y - q.y) < 1e-3, "{def}: {p:?} {q:?} ({:?})", back.custom_def().map(|d| d.proj4.clone()));
        // And through a file, as GeoTIFF writes and reads it.
        let gt = GeoTransform { origin_x: p.x, origin_y: p.y, pixel_width: 1.0, pixel_height: -1.0 };
        let r = Raster::new("k", 1, 1, vec![vec![1.0]], gt, crs, None, ColorHint::Gray, "Float32").unwrap();
        let file = read(std::io::Cursor::new(write(&r, 0).unwrap()), "k").unwrap_or_else(|e| panic!("{def}: {e}"));
        let q = file.crs.transformer().unwrap().from_lonlat(at).unwrap();
        assert!((p.x - q.x).hypot(p.y - q.y) < 1e-3, "{def}: file {p:?} {q:?}");
        (k, dd, a, back)
    }

    /// Orthographic and Robinson systems from WKT are written as keys (CT_Orthographic 21 and
    /// CT_Robinson 23, by the projection centre's keys) and read back as the same; an ellipsoidal
    /// equidistant cylindrical one (EPSG:4087's, CT 17) uses PROJ-compatible coordinates.
    #[test]
    fn orthographic_robinson_and_ellipsoidal_eqc_are_written_as_keys() {
        for (def, ct) in [
            ("+proj=ortho +lat_0=55 +lon_0=5 +x_0=100 +y_0=-200 +datum=WGS84 +units=m +no_defs", 21),
            ("+proj=ortho +lat_0=-90 +lon_0=0 +x_0=0 +y_0=0 +a=1737400 +b=1737400 +units=m +no_defs", 21),
            ("+proj=robin +lon_0=150 +x_0=1000 +y_0=-2000 +datum=WGS84 +units=m +no_defs", 23),
            ("+proj=eqc +lat_ts=30 +lon_0=-90 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs", 17),
            ("+proj=eqc +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +a=3396190 +rf=169.894447223612 +units=m +no_defs", 17),
        ] {
            let (k, d, a, _) = keys_round_trip(def);
            assert_eq!(geokey(&k, &d, &a, 3075).map(|v| matches!(v, KeyValue::Short(c) if c == ct)), Some(true), "{def}");
        }
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

    /// #1965: an overview a pixel short of every k-th pixel of the full image, as a writer that
    /// rounds its overviews' sizes down makes it (terrio's 10439 × 9842 for 83517 × 78739 over 8),
    /// is read rather than the whole full image, whose every k-th pixel is only a pixel larger. A
    /// 10 GB mosaic took minutes to open so.
    #[test]
    fn an_overview_rounded_down_is_read_rather_than_the_full_image() {
        use super::super::cog::{Example, example_with};
        // 1001 pixels a side in 256-pixel tiles, overviews of 500 and 250. At most 260,000
        // samples: every second pixel is 501 × 501, the 500 overview a pixel short each way.
        let file = example_with(&Example { size: 1001, tile: 256, rounded_down: true, ..Example::default() });
        let max = 260_000;
        let whole = read_limited(reader(std::io::Cursor::new(&file)).unwrap(), "rounded", max, None).unwrap();
        assert_eq!((whole.width, whole.height, whole.overview), (500, 500, Some((1001, 1001))));
        // Only the overview's tiles are read, none of the full image's (most of the file).
        let (r, parts) = read_in_parts(&file, 64 << 10, max);
        assert_eq!((r.width, r.height), (500, 500));
        assert!(parts.held() < file.len() as u64 / 3, "{} of {}", parts.held(), file.len());
        // One rounded up, as GDAL writes it, is read as before.
        let up = example_with(&Example { size: 1001, tile: 256, ..Example::default() });
        let r = read_limited(reader(std::io::Cursor::new(&up)).unwrap(), "up", max, None).unwrap();
        assert_eq!((r.width, r.height), (501, 501));
    }

    /// A part of a tiled file (#2865) is the pixels, place, no-data, system and band names of
    /// the whole file read at the same level then cut to it: the full image when the part fits,
    /// the finest overview whose part does when it doesn't, and every other pixel of a file
    /// without overviews (a file whose size two doesn't divide too, whose last pixel read passes
    /// its edge). It covers all the ground asked for, says where it lies in the grid read, and
    /// only the tiles that hold it are read.
    #[test]
    fn a_part_is_the_whole_file_read_at_its_level_then_cut_to_it() {
        use super::super::Part;
        use super::super::cog::{Example, example_with};
        /// A file's reader, counting the bytes read from it.
        struct Counting(std::io::Cursor<Vec<u8>>, std::rc::Rc<std::cell::Cell<u64>>);
        impl Read for Counting {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.read(buf)?;
                self.1.set(self.1.get() + n as u64);
                Ok(n)
            }
        }
        impl Seek for Counting {
            fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
                self.0.seek(to)
            }
        }
        let base = Example { size: 1024, tile: 128, ..Example::default() };
        let cases = [
            Example { bands: 3, wavelengths: true, ..base },
            Example { bands: 2, planar: true, ..base },
            Example { int8: true, ..base },
            Example { bottom_up: true, ..base },
            Example { no_overviews: true, ..base },
            Example { size: 1001, no_overviews: true, ..base },
        ];
        // The parts: one inside, and one reaching the image's right and bottom edges.
        let parts = [Part::new(0.3, 0.55, 0.42, 0.9).unwrap(), Part::new(0.987, 0.9, 1.0, 1.0).unwrap()];
        for e in cases {
            let file = example_with(&e);
            let (bands, size) = (usize::from(e.bands), e.size as usize);
            for part in parts {
                // The part fits at full size; then only at half size (an overview, or every other
                // pixel); the whole file is read at that size to compare.
                let (pw, ph) = part.span((e.size, e.size), (f64::from(e.size), f64::from(e.size))).size();
                let at_full = pw as usize * ph as usize * bands;
                for (part_max, whole_max, half) in
                    [(at_full, size * size * bands, false), (at_full - 1, size.div_ceil(2).pow(2) * bands, true)]
                {
                    let read = std::rc::Rc::new(std::cell::Cell::new(0));
                    let p = read_part(reader(Counting(std::io::Cursor::new(file.clone()), read.clone())).unwrap(), "part", part_max, part)
                        .unwrap()
                        .expect("read by part");
                    let whole = read_limited(reader(std::io::Cursor::new(&file)).unwrap(), "whole", whole_max, None).unwrap();
                    let r = &p.raster;
                    let at = format!("{e:?}, {part:?}, half {half}");
                    assert_eq!(whole.width as usize, if half { size.div_ceil(2) } else { size }, "{at}");
                    assert_eq!(p.grid, (whole.transform, whole.width, whole.height), "{at}");
                    let (c0, r0) = (i64::from(p.at.0), i64::from(p.at.1));
                    let gt = r.transform;
                    assert!((gt.origin_x - whole.transform.to_world(c0 as f64, r0 as f64).x).abs() < 1e-9, "{at}");
                    assert_eq!((gt.pixel_width, gt.pixel_height), (whole.transform.pixel_width, whole.transform.pixel_height));
                    // Every place asked for is on it: the part of the full image's ground, in the
                    // grid's pixels (the full image's size over how many of its pixels one spans).
                    let (sx, sy) = whole.file_pixels();
                    let (ex, ey) = (f64::from(e.size) / sx, f64::from(e.size) / sy);
                    assert!(c0 as f64 <= part.left * ex + 1e-9, "{at}: starts past the part");
                    assert!(c0 as f64 + f64::from(r.width) >= part.right * ex - 1e-9, "{at}: ends before the part does");
                    assert!(r0 as f64 <= part.top * ey + 1e-9, "{at}: starts below the part");
                    assert!(r0 as f64 + f64::from(r.height) >= part.bottom * ey - 1e-9, "{at}: ends above the part's end");
                    assert_eq!(
                        (r.crs, r.nodata, r.color.clone(), r.data_type.clone(), r.step),
                        (whole.crs, whole.nodata, whole.color.clone(), whole.data_type.clone(), whole.step)
                    );
                    let names = |r: &Raster| r.bands.iter().map(|b| (b.name.clone(), b.wavelength)).collect::<Vec<_>>();
                    assert_eq!(names(r), names(&whole), "{at}");
                    for b in 0..r.band_count() {
                        for y in 0..i64::from(r.height) {
                            for x in 0..i64::from(r.width) {
                                assert_eq!(r.value(b, x, y), whole.value(b, x + c0, y + r0), "{at}: band {b} at {x},{y}");
                            }
                        }
                    }
                    // Its size in the full image's pixels when coarser, as an overview of it.
                    let scale = |n: u32, by: f64| (f64::from(n) * by).round() as u32;
                    assert_eq!(r.overview, half.then(|| (scale(r.width, sx), scale(r.height, sy))), "{at}");
                    assert!(read.get() < file.len() as u64 / 4, "{at}: {} bytes of {} read", read.get(), file.len());
                }
            }
        }
    }

    /// A rotated file is resampled north up whole, and one without coordinates may be placed by a
    /// world file: neither is read by part, for the caller to read whole.
    #[test]
    fn a_turned_or_unplaced_file_is_not_read_by_part() {
        let part = super::super::Part::new(0.0, 0.0, 0.5, 0.5).unwrap();
        let data: Vec<u8> = (0..64 * 64).map(|i| (i % 251) as u8).collect();
        // A TIFF with no GeoTIFF tags: a scan.
        let mut scan = std::io::Cursor::new(Vec::new());
        tiff::encoder::TiffEncoder::new(&mut scan).unwrap().write_image::<tiff::encoder::colortype::Gray8>(64, 64, &data).unwrap();
        let scan = scan.into_inner();
        // Georeferenced, the same pixels are.
        assert!(read_part(reader(std::io::Cursor::new(grey8(64, 64, &data, None))).unwrap(), "placed", 1 << 20, part).unwrap().is_some());
        assert!(read_part(reader(std::io::Cursor::new(&scan)).unwrap(), "scan", 1 << 20, part).unwrap().is_none());
        let m = [30.0, 5.0, 0.0, 400_000.0, 3.0, -30.0, 0.0, 5_000_000.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let turned = tests_support::matrix_tiff(m, false);
        assert!(read_part(reader(std::io::Cursor::new(&turned)).unwrap(), "turned", 1 << 20, part).unwrap().is_none());
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
        rgbi_tiled_encoded(images, 8, 1, |b, x, y| u16::from(pixel(b, x, y)))
    }

    fn rgbi_tiled_encoded(images: &[(u32, u32)], bits: u16, predictor: u16, pixel: impl Fn(u32, u32, u32) -> u16) -> Vec<u8> {
        rgb_tiled_samples(&images.iter().map(|&(size, tile)| (size, tile, 4)).collect::<Vec<_>>(), bits, predictor, pixel)
    }

    fn rgb_tiled_samples(images: &[(u32, u32, u16)], bits: u16, predictor: u16, pixel: impl Fn(u32, u32, u32) -> u16) -> Vec<u8> {
        // The first image is the full one, the rest overviews (NewSubfileType 1).
        let images: Vec<(u32, u32, u16, u32)> =
            images.iter().enumerate().map(|(i, &(size, tile, samples))| (size, tile, samples, u32::from(i > 0))).collect();
        rgb_tiled_images(&images, bits, predictor, pixel)
    }

    /// As [`rgb_tiled_samples`], each image `(size, tile, samples, subfile type)` in turn: a
    /// transparency mask has the subfile type 4.
    fn rgb_tiled_images(images: &[(u32, u32, u16, u32)], bits: u16, predictor: u16, pixel: impl Fn(u32, u32, u32) -> u16) -> Vec<u8> {
        rgb_tiled_masked(images, bits, predictor, pixel, |_, _, _| true)
    }

    /// As [`rgb_tiled_images`], with a real mask for each image whose subfile type has bit 2 set:
    /// 1 bit a pixel, opaque where `opaque(size, x, y)` (in the mask's own pixels) holds, deflated unless
    /// `predictor` is 1.
    fn rgb_tiled_masked(
        images: &[(u32, u32, u16, u32)],
        bits: u16,
        predictor: u16,
        pixel: impl Fn(u32, u32, u32) -> u16,
        opaque: impl Fn(u32, u32, u32) -> bool,
    ) -> Vec<u8> {
        let full = images.first().map_or(1, |i| i.0);
        let mut out = vec![b'I', b'I', 42, 0, 0, 0, 0, 0];
        let mut next_link = 4;
        for (i, &(size, tile, samples, subfile)) in images.iter().enumerate() {
            let k = full / size;
            let (mask, deflated) = (subfile & 4 != 0, predictor != 1);
            let (samples, bits, predictor) = if mask { (1, 1, 1) } else { (samples, bits, predictor) };
            let n = size.div_ceil(tile);
            // The tiles' data first, then the IFD pointing at it.
            let mut offsets = Vec::new();
            let mut counts = Vec::new();
            for t in 0..n * n {
                offsets.push(out.len() as u32);
                let mut raw = Vec::new();
                for y in 0..tile {
                    if mask {
                        let y = (t / n) * tile + y;
                        let mut row = vec![0u8; tile.div_ceil(8) as usize];
                        for x in (0..tile).filter(|x| (t % n) * tile + x < size && y < size) {
                            if opaque(size, (t % n) * tile + x, y) {
                                row[(x / 8) as usize] |= 0x80 >> (x % 8);
                            }
                        }
                        raw.extend(row);
                        continue;
                    }
                    let mut prev = [0u16; 4];
                    for x in 0..tile {
                        let (x, y) = ((t % n) * tile + x, (t / n) * tile + y);
                        for b in 0..u32::from(samples) {
                            let value = if x < size && y < size { pixel(b, x * k, y * k) } else { 0 };
                            let encoded = if predictor == 2 { value.wrapping_sub(prev[b as usize]) } else { value };
                            prev[b as usize] = value;
                            raw.extend_from_slice(&encoded.to_le_bytes()[..usize::from(bits / 8)]);
                        }
                    }
                }
                let encoded = if predictor == 1 && !(mask && deflated) { raw } else { miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6) };
                counts.push(encoded.len() as u32);
                out.extend(encoded);
            }
            // Out-of-line values: (tag, type, count, bytes); type 3 short, 4 long, 12 double.
            let shorts = |v: &[u16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
            let longs = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
            let doubles = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
            let mut e: Vec<(u16, u16, u32, Vec<u8>)> = vec![
                (254, 4, 1, longs(&[subfile])),
                (256, 4, 1, longs(&[size])),
                (257, 4, 1, longs(&[size])),
                (258, 3, u32::from(samples), shorts(&vec![bits; usize::from(samples)])),
                (259, 3, 1, shorts(&[if predictor == 1 && !(mask && deflated) { 1 } else { 8 }])),
                (262, 3, 1, shorts(&[if mask { 4 } else { 2 }])),
                (277, 3, 1, shorts(&[samples])),
                (284, 3, 1, shorts(&[1])),
                (317, 3, 1, shorts(&[predictor])),
                (322, 4, 1, longs(&[tile])),
                (323, 4, 1, longs(&[tile])),
                (324, 4, offsets.len() as u32, longs(&offsets)),
                (325, 4, counts.len() as u32, longs(&counts)),
            ];
            if samples == 4 {
                e.push((338, 3, 1, shorts(&[0])));
            }
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
    fn rgbi_header_patches_handle_both_byte_orders_and_tiff_sizes() {
        for big in [false, true] {
            for little in [false, true] {
                let short = |v: u16| if little { v.to_le_bytes() } else { v.to_be_bytes() };
                let long = |v: u32| if little { v.to_le_bytes() } else { v.to_be_bytes() };
                let wide = |v: u64| if little { v.to_le_bytes() } else { v.to_be_bytes() };
                let mut bytes = if little { b"II".to_vec() } else { b"MM".to_vec() };
                bytes.extend(short(if big { 43 } else { 42 }));
                let at = if big { 16 } else { 8 };
                if big {
                    bytes.extend(short(8));
                    bytes.extend(short(0));
                    bytes.extend(wide(at));
                    bytes.extend(wide(3));
                } else {
                    bytes.extend(long(at as u32));
                    bytes.extend(short(3));
                }
                let photo_at = bytes.len() + if big { 12 } else { 8 };
                for (tag, value) in [(262, 2), (277, 4), (338, 0)] {
                    bytes.extend(short(tag));
                    bytes.extend(short(3));
                    bytes.extend(if big { wide(1).to_vec() } else { long(1).to_vec() });
                    bytes.extend(short(value));
                    bytes.extend(vec![0; if big { 6 } else { 2 }]);
                }
                bytes.extend(vec![0; if big { 8 } else { 4 }]);
                let original = bytes.clone();
                let mut r = Unpaletted::new(std::io::Cursor::new(bytes)).unwrap();
                assert!(r.first_is_wide_rgb());
                assert!(!r.first_is_palette());
                let mut patched = Vec::new();
                // Reading a byte at a time exercises patches split between reads.
                let mut byte = [0u8; 1];
                while r.read(&mut byte).unwrap() != 0 {
                    patched.push(byte[0]);
                }
                let mut want = original.clone();
                want[photo_at..photo_at + 2].copy_from_slice(&short(1));
                assert_eq!(patched, want);
                assert_eq!(r.inner.into_inner(), original);
            }
        }
    }

    /// A SampleFormat of 4 (undefined) is shown to the decoder as 1, unsigned (#2593): in the
    /// entry when its values fit there, else in the array it points to, in both byte orders and
    /// both TIFF sizes. The file itself is unchanged.
    #[test]
    fn void_sample_format_patches_handle_both_byte_orders_and_tiff_sizes() {
        for big in [false, true] {
            for little in [false, true] {
                for samples in [1u16, 3, 5] {
                    let short = |v: u16| if little { v.to_le_bytes() } else { v.to_be_bytes() };
                    let long = |v: u32| if little { v.to_le_bytes() } else { v.to_be_bytes() };
                    let wide = |v: u64| if little { v.to_le_bytes() } else { v.to_be_bytes() };
                    let count = |v: usize| if big { wide(v as u64).to_vec() } else { long(v as u32).to_vec() };
                    let mut bytes = if little { b"II".to_vec() } else { b"MM".to_vec() };
                    bytes.extend(short(if big { 43 } else { 42 }));
                    let at = if big { 16 } else { 8 };
                    if big {
                        bytes.extend(short(8));
                        bytes.extend(short(0));
                        bytes.extend(wide(at));
                        bytes.extend(wide(3));
                    } else {
                        bytes.extend(long(at as u32));
                        bytes.extend(short(3));
                    }
                    let (entry_len, field) = if big { (20, 8) } else { (12, 4) };
                    let entries_at = bytes.len();
                    for (tag, value) in [(262, 1), (277, samples)] {
                        bytes.extend(short(tag));
                        bytes.extend(short(3));
                        bytes.extend(count(1));
                        bytes.extend(short(value));
                        bytes.extend(vec![0; field - 2]);
                    }
                    // SampleFormat: 4 for each sample, in the entry or after the IFD.
                    let n = usize::from(samples);
                    let values: Vec<u8> = (0..n).flat_map(|_| short(4)).collect();
                    let inline = 2 * n <= field;
                    let array_at = entries_at + 3 * entry_len + field;
                    bytes.extend(short(339));
                    bytes.extend(short(3));
                    bytes.extend(count(n));
                    if inline {
                        bytes.extend(&values);
                        bytes.extend(vec![0; field - values.len()]);
                    } else {
                        bytes.extend(count(array_at));
                    }
                    bytes.extend(vec![0; field]);
                    if !inline {
                        bytes.extend(&values);
                    }
                    let original = bytes.clone();
                    let mut r = Unpaletted::new(std::io::Cursor::new(bytes)).unwrap();
                    assert!(!r.first_is_wide_rgb() && !r.first_is_palette());
                    let mut patched = Vec::new();
                    // Reading a byte at a time exercises patches split between reads.
                    let mut byte = [0u8; 1];
                    while r.read(&mut byte).unwrap() != 0 {
                        patched.push(byte[0]);
                    }
                    let mut want = original.clone();
                    let first = if inline { entries_at + 2 * entry_len + if big { 12 } else { 8 } } else { array_at };
                    for j in 0..n {
                        want[first + 2 * j..first + 2 * j + 2].copy_from_slice(&short(1));
                    }
                    assert_eq!(patched, want, "big {big}, little {little}, {samples} samples");
                    assert_eq!(r.inner.into_inner(), original);
                }
            }
        }
    }

    /// Sets every SampleFormat value in a little-endian classic TIFF to `format`, in the entry or in
    /// the array it points to, each image's; returns how many it set.
    fn set_sample_format(file: &mut [u8], format: u16) -> usize {
        let short = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
        let long = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize;
        let (mut ifd, mut set) = (long(file, 4), 0);
        while ifd != 0 {
            let n = usize::from(short(file, ifd));
            let entries: Vec<usize> = (0..n).map(|i| ifd + 2 + 12 * i).filter(|e| short(file, *e) == 339).collect();
            for e in entries {
                let count = long(file, e + 4);
                let at = if count <= 2 { e + 8 } else { long(file, e + 8) };
                for j in 0..count {
                    file[at + 2 * j..at + 2 * j + 2].copy_from_slice(&format.to_le_bytes());
                    set += 1;
                }
            }
            ifd = long(file, ifd + 2 + 12 * n);
        }
        set
    }

    /// A GeoTIFF whose SampleFormat is 4, undefined (the CBERS-4 scenes of the Brazil Data Cube),
    /// reads as unsigned whole numbers, as libtiff and GDAL read it (#2593): the same type and
    /// values as the file with 1. A one-band 8-bit Deflate image in strips; a tiled three-band one
    /// with overviews, whose SampleFormat values are arrays apart from their entries, whole and a
    /// tile at a time as a COG window reads it.
    #[test]
    fn a_void_sample_format_reads_as_unsigned() {
        use tiff::encoder::{Compression, DeflateLevel, TiffEncoder, colortype};
        let mut strips = Vec::new();
        TiffEncoder::new(std::io::Cursor::new(&mut strips))
            .unwrap()
            .with_compression(Compression::Deflate(DeflateLevel::Balanced))
            .write_image::<colortype::Gray8>(2, 2, &[3, 70, 140, 255])
            .unwrap();
        let (w, h) = (600u32, 600u32);
        let bands = (0..3).map(|b| (0..w * h).map(|i| ((i + 40 * b) % 256) as f32).collect()).collect();
        let gt = GeoTransform { origin_x: 10.0, origin_y: 46.0, pixel_width: 0.001, pixel_height: -0.001 };
        let rgb = Raster::new("rgb", w, h, bands, gt, Crs::LonLat, None, ColorHint::Rgb { alpha: false }, "Byte").unwrap();
        let tiled = crate::raster::cogwrite::write(&rgb, 0).unwrap();
        for (name, file, values) in [("strips", strips, 1), ("tiled", tiled, 6)] {
            let mut void = file.clone();
            assert!(set_sample_format(&mut void, 4) >= values, "{name}: SampleFormat set");
            let want = read(std::io::Cursor::new(&file), name).unwrap();
            let got = read(std::io::Cursor::new(&void), name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!((got.data_type.as_str(), got.band_count()), (want.data_type.as_str(), want.band_count()), "{name}");
            assert!(got.bands.iter().zip(&want.bands).all(|(a, b)| a.data == b.data), "{name}: the values");
            if name == "strips" {
                continue;
            }
            // A tile at a time, each level, as a COG window reads it. Compared bit for bit: an
            // overview's tile past its image's edge is NaN.
            let layout = |file: &[u8]| match super::super::cog::layout(&Sparse::default().with(0, file.to_vec())).unwrap() {
                super::super::cog::Parse::Done(l) => l,
                _ => panic!("{name}: not read a tile at a time"),
            };
            let (l, lv) = (layout(&void), layout(&file));
            assert!(l.levels.len() > 1, "{name}: overviews");
            for level in 0..l.levels.len() {
                let (at, n) = (l.levels[level].offsets[0] as usize, l.levels[level].counts[0] as usize);
                let tile = |file: &[u8], l: &super::super::cog::Layout| {
                    let bands =
                        super::super::cog::decode_tile(&Sparse::default().with(0, file.to_vec()), l, level, 0, file[at..at + n].to_vec())
                            .unwrap_or_else(|e| panic!("level {level}: {e}"));
                    bands.into_iter().map(|b| b.into_iter().map(f32::to_bits).collect::<Vec<u32>>()).collect::<Vec<_>>()
                };
                assert_eq!(tile(&void, &l), tile(&file, &lv), "level {level}");
            }
        }
    }

    #[test]
    fn rgbi_strips_preserve_samples_and_real_alpha_is_unchanged() {
        use tiff::encoder::{Compression, DeflateLevel, TiffEncoder, colortype};
        fn image<K: tiff::encoder::TiffKind>(mut enc: TiffEncoder<std::io::Cursor<&mut Vec<u8>>, K>, extra: u16) {
            let mut image = enc.new_image::<colortype::RGBA16>(7, 5).unwrap();
            image.encoder().write_tag(Tag::ExtraSamples, &[extra][..]).unwrap();
            image.rows_per_strip(2).unwrap();
            let values: Vec<u16> = (0..7 * 5).flat_map(|p| [1000 + p * 11, 2000 + p * 13, 3000 + p * 17, p % 3]).collect();
            image.write_data(&values).unwrap();
        }
        for big in [false, true] {
            for extra in [0, 1, 2] {
                let mut bytes = Vec::new();
                if big {
                    image(
                        TiffEncoder::new_big(std::io::Cursor::new(&mut bytes))
                            .unwrap()
                            .with_compression(Compression::Deflate(DeflateLevel::Balanced))
                            .with_predictor(tiff::encoder::Predictor::Horizontal),
                        extra,
                    );
                } else {
                    image(
                        TiffEncoder::new(std::io::Cursor::new(&mut bytes))
                            .unwrap()
                            .with_compression(Compression::Deflate(DeflateLevel::Balanced))
                            .with_predictor(tiff::encoder::Predictor::Horizontal),
                        extra,
                    );
                }
                let r = read(std::io::Cursor::new(&bytes), "strips").unwrap();
                assert_eq!(r.color, ColorHint::Rgb { alpha: extra != 0 });
                assert_eq!(r.band_count(), 4);
                for p in 0u16..7 * 5 {
                    for (b, value) in [1000 + p * 11, 2000 + p * 13, 3000 + p * 17, p % 3].iter().enumerate() {
                        assert_eq!(r.value(b, (p % 7).into(), (p / 7).into()), Some(f32::from(*value)));
                    }
                }
            }
        }
    }

    /// Sets the ExtraSamples value of each image with one, in turn, of a file
    /// [`rgb_tiled_samples`] wrote.
    fn set_extra_samples(bytes: &mut [u8], values: &[u16]) {
        // The tag's entry: 338, type 3 (short), count 1, the value 0 as written.
        let entry = [0x52, 0x01, 3, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        let at: Vec<usize> = (0..bytes.len().saturating_sub(12)).filter(|i| bytes[*i..*i + 12] == entry).collect();
        assert_eq!(at.len(), values.len());
        for (at, value) in at.iter().zip(values) {
            bytes[at + 8..at + 10].copy_from_slice(&value.to_le_bytes());
        }
    }

    /// An overview with the full image's sample count but another alpha role (an RGBA overview of
    /// an RGBI image, or the reverse) is left out and said, not decoded with the full image's
    /// colour layout; one with the same role is a level, with nothing said.
    #[test]
    fn cog_overviews_require_a_matching_alpha_sample() {
        use super::super::cog::{self, Parse, Sparse};
        for (full, overview) in [(0, 0), (2, 2), (0, 2), (2, 0)] {
            let mut bytes = rgb_tiled_samples(&[(40, 16, 4), (20, 16, 4)], 8, 1, |b, x, y| (b * 10 + x + y) as u16);
            set_extra_samples(&mut bytes, &[full, overview]);
            let whole = Sparse::default().with_len(bytes.len() as u64).with(0, bytes);
            let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
            let said: Vec<_> = layout.lost.iter().filter(|n| n.contains("overview(s)")).collect();
            if full == overview {
                assert_eq!(layout.levels.len(), 2);
                assert!(said.is_empty(), "{:?}", layout.lost);
            } else {
                assert_eq!(layout.levels.len(), 1);
                let role = if overview == 2 { "image 1 (4, with alpha)" } else { "image 1 (4, without alpha)" };
                assert!(said.len() == 1 && said[0].contains("an alpha sample") && said[0].contains(role), "{:?}", layout.lost);
            }
        }
    }

    /// The scan stops at its most levels, and says so when the file holds more images; a file
    /// with fewer says nothing of it.
    #[test]
    fn cog_says_when_it_stops_at_its_most_levels() {
        use super::super::cog::{self, Parse, Sparse};
        const STOPPED: &str = "the scan for overviews stopped at 32 levels";
        for (images, cut) in [(32u32, false), (33, true)] {
            // The full image 64 wide, its overviews 63, 62, … wide, smallest last.
            let sizes: Vec<(u32, u32, u16)> = (0..images).map(|k| (64 - k, 16, 3)).collect();
            let bytes = rgb_tiled_samples(&sizes, 8, 1, |b, x, y| (b + x + y) as u16);
            let whole = Sparse::default().with_len(bytes.len() as u64).with(0, bytes);
            let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
            assert_eq!(layout.levels.len(), 32.min(images as usize));
            assert_eq!(layout.lost.iter().any(|n| n.contains(STOPPED)), cut, "{:?}", layout.lost);
        }
    }

    #[test]
    fn cog_overviews_require_matching_sample_counts() {
        use super::super::cog::{self, Parse, Sparse};
        for full in [3, 4] {
            for overview in [3, 4] {
                let bytes = rgb_tiled_samples(&[(40, 16, full), (20, 16, overview)], 8, 1, |b, x, y| (b * 10 + x + y) as u16);
                let whole = Sparse::default().with_len(bytes.len() as u64).with(0, bytes);
                if full == overview {
                    let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
                    assert_eq!(layout.levels.len(), 2);
                    assert_eq!(layout.bands, usize::from(full));
                    assert!(layout.lost.iter().all(|n| !n.contains("overview(s)")), "{:?}", layout.lost);
                } else {
                    // The overview is left out, and said; the full image is still read.
                    let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
                    assert_eq!((layout.levels.len(), layout.bands), (1, usize::from(full)));
                    assert!(
                        layout.lost.iter().any(|n| n.contains(&format!(
                            "1 overview(s) with samples per pixel or an alpha sample other than the full image's ({full})"
                        )) && n.contains(&format!("image 1 ({overview})"))),
                        "{:?}",
                        layout.lost
                    );
                }
            }
        }
        // An image smaller than the full one is an overview candidate wherever it sits, so one
        // whose samples differ is left out and said, rather than silently passed over.
        let bytes = rgb_tiled_samples(&[(40, 16, 4), (20, 16, 4), (20, 16, 3)], 8, 1, |b, x, y| (b * 10 + x + y) as u16);
        let whole = Sparse::default().with_len(bytes.len() as u64).with(0, bytes);
        let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
        assert_eq!((layout.levels.len(), layout.bands), (2, 4));
        assert!(
            layout
                .lost
                .iter()
                .any(|n| n.contains("1 overview(s) with samples per pixel or an alpha sample other than the full image's (4)")
                    && n.contains("image 2 (3)")),
            "{:?}",
            layout.lost
        );
        // An image no smaller than the full one in both directions isn't an overview: passed over,
        // and said, whatever its samples.
        let bytes = rgb_tiled_samples(&[(40, 16, 4), (40, 16, 4), (20, 16, 4)], 8, 1, |b, x, y| (b * 10 + x + y) as u16);
        let whole = Sparse::default().with_len(bytes.len() as u64).with(0, bytes);
        let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
        assert_eq!(layout.levels.iter().map(|l| l.width).collect::<Vec<_>>(), [40, 20]);
        assert!(
            layout.lost.iter().any(|n| n.contains("1 image(s) not smaller than the full image") && n.contains("image 1 (40 × 40)")),
            "{:?}",
            layout.lost
        );
        // Each size is one level: an overview as wide as one kept is passed over, and said.
        let bytes = rgb_tiled_samples(&[(40, 16, 4), (20, 16, 4), (20, 16, 4)], 8, 1, |b, x, y| (b * 10 + x + y) as u16);
        let whole = Sparse::default().with_len(bytes.len() as u64).with(0, bytes);
        let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
        assert_eq!(layout.levels.iter().map(|l| l.width).collect::<Vec<_>>(), [40, 20]);
        assert!(
            layout.lost.iter().any(|n| n.contains("1 overview(s) as wide as one already kept") && n.contains("image 2 (20 × 20)")),
            "{:?}",
            layout.lost
        );
        // A left-out overview's tiles, too large to be a level's, don't fail the file: the next
        // overview with the full image's samples is still a level.
        let bytes = rgb_tiled_samples(&[(40, 16, 4), (20, 1200, 3), (10, 16, 4)], 8, 1, |b, x, y| (b * 10 + x + y) as u16);
        let whole = Sparse::default().with_len(bytes.len() as u64).with(0, bytes);
        let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
        assert_eq!(layout.levels.iter().map(|l| (l.width, l.ifd)).collect::<Vec<_>>(), [(40, 0), (10, 2)]);
        assert!(layout.lost.iter().any(|n| n.contains("1 overview(s)") && n.contains("image 1 (3)")), "{:?}", layout.lost);
        assert!(layout.lost.iter().all(|n| !n.contains("scan for overviews stopped")), "{:?}", layout.lost);
    }

    /// Looking for overviews passes over a bounded number of images, and says when it stops.
    #[test]
    fn cog_overview_scan_stops_after_so_many_skipped_images() {
        use super::super::cog::{self, Parse, Sparse};
        // The full image, 64 more of its size (not overviews of it), then an overview.
        let mut images = vec![(32, 16, 4); 65];
        images.push((16, 16, 4));
        let bytes = rgb_tiled_samples(&images, 8, 1, |b, x, y| (b * 10 + x + y) as u16);
        let whole = Sparse::default().with_len(bytes.len() as u64).with(0, bytes);
        let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
        assert_eq!(layout.levels.iter().map(|l| l.width).collect::<Vec<_>>(), [32]);
        assert!(layout.lost.iter().any(|n| n.contains("stopped after 64 skipped images")), "{:?}", layout.lost);
    }

    /// Transparency masks count toward the same scan bound as the other images passed over.
    #[test]
    fn cog_overview_scan_stops_after_so_many_masks() {
        use super::super::cog::{self, Parse, Sparse};
        // The full image, 64 masks of its size, then an overview the scan never reaches.
        let mut images = vec![(32, 16, 4, 0)];
        images.extend(vec![(32, 16, 4, 4); 64]);
        images.push((16, 16, 4, 1));
        let bytes = rgb_tiled_images(&images, 8, 1, |b, x, y| (b * 10 + x + y) as u16);
        let whole = Sparse::default().with_len(bytes.len() as u64).with(0, bytes);
        let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
        assert_eq!(layout.levels.iter().map(|l| l.width).collect::<Vec<_>>(), [32]);
        assert!(layout.lost.iter().any(|n| n.contains("stopped after 64 skipped images")), "{:?}", layout.lost);
        // Fewer masks: the overview is read, and nothing is said.
        let mut images = vec![(32, 16, 4, 0)];
        images.extend(vec![(32, 16, 4, 4); 3]);
        images.push((16, 16, 4, 1));
        let bytes = rgb_tiled_images(&images, 8, 1, |b, x, y| (b * 10 + x + y) as u16);
        let whole = Sparse::default().with_len(bytes.len() as u64).with(0, bytes);
        let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
        assert_eq!(layout.levels.iter().map(|l| (l.width, l.ifd)).collect::<Vec<_>>(), [(32, 0), (16, 4)]);
        assert!(layout.lost.iter().all(|n| !n.contains("scan for overviews stopped")), "{:?}", layout.lost);
    }

    /// Sets `tag`'s value (a short, in place) in the `ifd`-th image of a little-endian TIFF.
    fn set_short(bytes: &mut [u8], ifd: usize, tag: u16, value: u16) {
        let long = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize;
        let mut at = long(bytes, 4);
        for _ in 0..ifd {
            at = long(bytes, at + 2 + 12 * usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]])));
        }
        for k in 0..usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]])) {
            let e = at + 2 + 12 * k;
            if bytes[e..e + 2] == tag.to_le_bytes() {
                bytes[e + 8..e + 10].copy_from_slice(&value.to_le_bytes());
            }
        }
    }

    /// The issue's file: a JPEG RGB image's border masked by an internal mask (stored black), its
    /// centre kept, one black centre pixel included.
    fn masked_rgb(predictor: u16) -> Vec<u8> {
        let centre = |x: u32, y: u32| (8..24).contains(&x) && (8..24).contains(&y);
        let pixel = move |b: u32, x: u32, y: u32| if centre(x, y) && (x, y) != (12, 12) { [200, 120, 40][b as usize] } else { 0 };
        rgb_tiled_masked(&[(32, 16, 3, 0), (32, 16, 1, 4)], 8, predictor, pixel, move |_, x, y| centre(x, y))
    }

    #[test]
    fn an_internal_mask_makes_masked_pixels_transparent() {
        for predictor in [1, 2] {
            let r = read(std::io::Cursor::new(masked_rgb(predictor)), "masked").unwrap();
            assert_eq!((r.width, r.height, r.band_count(), r.color.clone()), (32, 32, 3, ColorHint::Rgb { alpha: false }));
            for y in 0..32 {
                for x in 0..32 {
                    for b in 0..3 {
                        let inside = (8..24).contains(&x) && (8..24).contains(&y);
                        let want = if !inside {
                            None
                        } else if (x, y) == (12, 12) {
                            Some(0.0)
                        } else {
                            Some([200.0, 120.0, 40.0][b])
                        };
                        assert_eq!(r.value(b, x as i64, y as i64), want, "band {b} at {x},{y}");
                    }
                }
            }
            assert!(r.lost.is_empty(), "{:?}", r.lost);
            // The statistics leave the masked pixels out.
            assert_eq!(r.bands[0].stats.count, 16 * 16);
        }
    }

    #[test]
    fn a_file_without_a_mask_is_unchanged() {
        let pixel = |b: u32, x: u32, y: u32| (b * 10 + x + y) as u16;
        let r = read(std::io::Cursor::new(rgb_tiled_samples(&[(32, 16, 3)], 8, 1, pixel)), "plain").unwrap();
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        assert_eq!(r.bands[0].stats.count, 32 * 32);
        assert_eq!(r.value(1, 0, 0), Some(10.0));
    }

    #[test]
    fn an_overviews_own_mask_applies_when_the_overview_is_read() {
        // The full image's mask hides its left 8 columns, the overview's its top 4 rows.
        let opaque = |size: u32, x: u32, y: u32| if size == 64 { x >= 8 } else { y >= 4 };
        let images = [(64, 16, 3, 0), (32, 16, 3, 1), (32, 16, 1, 5), (64, 16, 1, 4)];
        let bytes = rgb_tiled_masked(&images, 8, 1, |b, _, _| u16::try_from(b + 1).unwrap_or(0) * 50, opaque);
        // Masks may come in any order; 3 × 32 × 32 samples fit, 3 × 64 × 64 don't.
        let r = read_limited(reader(std::io::Cursor::new(&bytes)).unwrap(), "overview", 4_000, None).unwrap();
        assert_eq!((r.width, r.height, r.overview), (32, 32, Some((64, 64))));
        for (x, y) in [(0, 0), (31, 3), (0, 3)] {
            assert_eq!(r.value(0, x, y), None, "{x},{y}");
        }
        for (x, y) in [(0, 4), (3, 4), (31, 31), (2, 20)] {
            assert_eq!(r.value(0, x, y), Some(50.0), "{x},{y}");
        }
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        // The whole image has its own.
        let full = read(std::io::Cursor::new(&bytes), "full").unwrap();
        assert_eq!((full.width, full.value(0, 7, 40), full.value(0, 8, 0)), (64, None, Some(50.0)));
    }

    #[test]
    fn an_overview_without_a_mask_of_its_own_says_so() {
        // Only the full image has a mask: the overview read draws as stored, and says why.
        let images = [(64, 16, 3, 0), (32, 16, 3, 1), (64, 16, 1, 4)];
        let bytes = rgb_tiled_masked(&images, 8, 1, |_, _, _| 50, |_, x, _| x >= 8);
        let r = read_limited(reader(std::io::Cursor::new(&bytes)).unwrap(), "overview", 4_000, None).unwrap();
        assert_eq!((r.width, r.value(0, 0, 0)), (32, Some(50.0)));
        assert_eq!(
            r.lost,
            ["its transparency mask isn't applied (the file has none for the image read): masked pixels draw as stored"],
            "{:?}",
            r.lost
        );
    }

    #[test]
    fn a_mask_follows_an_image_read_every_other_pixel() {
        // No overview fits: every second pixel of the full image is read, with its mask's.
        let opaque = |_: u32, x: u32, y: u32| (x / 3 + y / 5).is_multiple_of(2);
        let bytes = rgb_tiled_masked(&[(64, 16, 3, 0), (64, 16, 1, 4)], 8, 1, |_, _, _| 90, opaque);
        let r = read_limited(reader(std::io::Cursor::new(&bytes)).unwrap(), "sparse", 4_000, None).unwrap();
        assert_eq!((r.width, r.overview), (32, Some((64, 64))));
        for y in 0..32u32 {
            for x in 0..32u32 {
                let want = opaque(64, x * 2, y * 2).then_some(90.0);
                assert_eq!(r.value(0, i64::from(x), i64::from(y)), want, "{x},{y}");
            }
        }
    }

    #[test]
    fn a_mask_that_cant_be_read_is_said_once_and_the_image_loads() {
        let mut bytes = masked_rgb(1);
        // JPEG, which masks aren't read in.
        set_short(&mut bytes, 1, 259, 7);
        let r = read(std::io::Cursor::new(&bytes), "bad").unwrap();
        assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
        assert!(r.lost[0].starts_with("its transparency mask isn't applied (a transparency mask's compression (7)"), "{:?}", r.lost);
        assert_eq!(r.value(0, 0, 0), Some(0.0), "masked pixels draw as stored");
        assert_eq!(r.value(0, 20, 20), Some(200.0));
        // More than one bit a pixel.
        let mut bytes = masked_rgb(1);
        set_short(&mut bytes, 1, 258, 8);
        let r = read(std::io::Cursor::new(&bytes), "bad").unwrap();
        assert!(r.lost.iter().any(|n| n.contains("isn't 1 bit a pixel")), "{:?}", r.lost);
        assert_eq!(r.value(0, 0, 0), Some(0.0));
    }

    #[test]
    fn a_cog_header_says_its_masks_arent_applied_in_place() {
        use super::super::cog::{self, Parse, Sparse};
        let layout = |bytes: Vec<u8>| {
            let whole = Sparse::default().with_len(bytes.len() as u64).with(0, bytes);
            let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
            layout
        };
        let l = layout(masked_rgb(1));
        assert_eq!(l.lost, ["1 transparency mask(s) aren't applied when tiles are read in place: masked pixels draw as stored"]);
        let plain = layout(rgb_tiled_samples(&[(32, 16, 3)], 8, 1, |b, _, _| b as u16));
        assert!(plain.lost.is_empty(), "{:?}", plain.lost);
    }

    /// An RGBI image too large to read whole, with a three-sample overview that would fit: the
    /// overview would lose the fourth band, so the full image is read sparsely instead.
    #[test]
    fn whole_file_reads_skip_overviews_with_other_sample_counts() {
        let pixel = |b: u32, x: u32, y: u32| (b * 10 + x + y) as u16;
        let bytes = rgb_tiled_samples(&[(40, 16, 4), (20, 16, 3)], 8, 1, pixel);
        let r = read_limited(reader(std::io::Cursor::new(&bytes)).unwrap(), "rgbi", 2_000, None).unwrap();
        assert_eq!((r.band_count(), r.color.clone()), (4, ColorHint::Rgb { alpha: false }));
        // Every other pixel of the full image, not the full image whole.
        assert_eq!((r.width, r.height, r.overview), (20, 20, Some((40, 40))));
        for y in 0..r.height {
            for x in 0..r.width {
                for b in 0..4 {
                    assert_eq!(
                        r.value(b as usize, i64::from(x), i64::from(y)),
                        Some(f32::from(pixel(b, x * 2, y * 2))),
                        "band {b} at {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn rgbi_cog_tiles_keep_the_fourth_band_without_alpha() {
        use super::super::cog::{self, Parse, Sparse};
        let pixel = |b: u32, x: u32, y: u32| (1000 + x * 19 + y * 37 + b * 53) as u16;
        let bytes = rgbi_tiled_encoded(&[(40, 16), (20, 16)], 16, 2, pixel);
        let whole = Sparse::default().with(0, bytes.clone()).with_len(bytes.len() as u64);
        let Parse::Done(layout) = cog::layout(&whole).unwrap() else { panic!("not a COG") };
        // Hold only the IFDs and their values, as a range reader does. This fixture writes each
        // IFD after its tiles and before the following level's tiles.
        let mut header = Sparse::default().with(0, bytes[..8].to_vec()).with_len(bytes.len() as u64);
        let mut ifd = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        for i in 0..layout.levels.len() {
            let count = u16::from_le_bytes(bytes[ifd..ifd + 2].try_into().unwrap()) as usize;
            let next_at = ifd + 2 + count * 12;
            let end = layout.levels.get(i + 1).map_or(bytes.len(), |l| l.offsets[0] as usize);
            header = header.with(ifd as u64, bytes[ifd..end].to_vec());
            ifd = u32::from_le_bytes(bytes[next_at..next_at + 4].try_into().unwrap()) as usize;
        }
        let Parse::Done(layout) = cog::layout(&header).unwrap() else { panic!("not a COG header") };
        assert_eq!(layout.bands, 4);
        assert_eq!(layout.color, ColorHint::Rgb { alpha: false });
        for (level, k) in [(0, 1), (1, 2)] {
            let l = &layout.levels[level];
            for t in 0..l.tiles() {
                let at = l.offsets[t as usize] as usize;
                let count = l.counts[t as usize] as usize;
                let bands = cog::decode_tile(&header, &layout, level, t, bytes[at..at + count].to_vec()).unwrap();
                assert_eq!(bands.len(), 4);
                for y in 0..l.tile_height {
                    for x in 0..l.tile_width {
                        let (px, py) = ((t % l.across()) * l.tile_width + x, (t / l.across()) * l.tile_height + y);
                        for (b, band) in bands.iter().enumerate() {
                            let v = band[(y * l.tile_width + x) as usize];
                            if px < l.width && py < l.height {
                                assert_eq!(v, f32::from(pixel(b as u32, px * k, py * k)));
                            } else {
                                assert!(v.is_nan());
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn horizontal_predictor_keeps_rgb_and_the_unspecified_fourth_band() {
        for bits in [8, 16] {
            let pixel = |b: u32, x: u32, y: u32| ((x * 19 + y * 37 + b * 53) % 251 + 1) as u16 * (if bits == 16 { 200 } else { 1 });
            let bytes = rgbi_tiled_encoded(&[(40, 16), (20, 16)], bits, 2, pixel);
            for (max, k) in [(100_000, 1), (2_000, 2)] {
                let r = read_limited(reader(std::io::Cursor::new(&bytes)).unwrap(), "rgbi", max, None).unwrap();
                assert_eq!(r.color, ColorHint::Rgb { alpha: false });
                assert_eq!(r.band_count(), 4);
                for y in 0..r.height {
                    for x in 0..r.width {
                        for b in 0..4 {
                            assert_eq!(
                                r.value(b as usize, x.into(), y.into()),
                                Some(f32::from(pixel(b, x * k, y * k))),
                                "{bits} bits, band {b}, pixel {x},{y}, scale {k}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn padded_tiles_with_an_extra_sample_stay_in_their_rows() {
        // Issue #162: an RGBI orthophoto (5000 pixels in 128-pixel tiles) opened with black
        // columns down its west edge, the padding of each row's last tile spilt into the next row.
        let pixel = |b: u32, x: u32, y: u32| ((x + 3 * y + 60 * b) % 250 + 1) as u8;
        let bytes = rgbi_tiled(&[(40, 16), (20, 16)], pixel);
        // Its near-infrared band is read too, the fourth, after red, green and blue.
        let check = |r: &Raster, k: u32| {
            assert_eq!((r.band_count(), r.color.clone()), (4, ColorHint::Rgb { alpha: false }));
            for y in 0..r.height {
                for x in 0..r.width {
                    for b in 0..4 {
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
            1u16, 1, 0, 12, 1024, 0, 1, 1, 1025, 0, 1, 1, 2048, 0, 1, 4326, 3072, 0, 1, 32767, 3074, 0, 1, 32767, 3075, 0, 1, 11, 3076, 0,
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

    /// Axes in kilometres converted to metres in slots of their own; past the last slot a key can
    /// address (65,535), refused rather than left in kilometres under a metre unit.
    #[test]
    fn axes_in_metres_need_room_for_their_slots() {
        let keys = [1u16, 1, 0, 3, 2052, 0, 1, 9036, 2057, 34736, 1, 0, 2058, 34736, 1, 1];
        let (k, d) = axes_in_metres(&keys, &[3396.19, 3376.2], "").unwrap();
        assert_eq!((d[2], d[3], k[7]), (3_396_190.0, 3_376_200.0, 9001));
        for len in [65_535, 65_536] {
            let mut doubles = vec![0.0; len];
            doubles[..2].copy_from_slice(&[3396.19, 3376.2]);
            let e = axes_in_metres(&keys, &doubles, "").unwrap_err().to_string();
            assert!(e.contains("too many double parameters"), "{len}: {e}");
        }
        // A semi-minor axis past the doubles isn't left in kilometres under a unit of metres.
        let e = axes_in_metres(&keys, &[3396.19], "").unwrap_err().to_string();
        assert!(e.contains("points past its double parameters"), "{e}");
    }

    /// An ESRI PE citation with a shift of its own (British National Grid with another TOWGS84) is
    /// read by that WKT, as the `.prj` is, not as EPSG:27700 with the code's shift; without one it
    /// is the code.
    #[test]
    fn a_citation_with_its_own_shift_is_not_its_epsg_code() {
        let bng = r#"PROJCS["British_National_Grid",GEOGCS["GCS_OSGB_1936",DATUM["D_OSGB_1936",SPHEROID["Airy_1830",6377563.396,299.3249646],TOWGS84[375,-111,431,0,0,0,0]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",400000.0],PARAMETER["False_Northing",-100000.0],PARAMETER["Central_Meridian",-2.0],PARAMETER["Scale_Factor",0.9996012717],PARAMETER["Latitude_Of_Origin",49.0],UNIT["Meter",1.0]]"#;
        let keys = |ascii: &str| vec![1, 1, 0, 3, 1024, 0, 1, 1, 1026, 34737, u16::try_from(ascii.len()).unwrap(), 0, 3072, 0, 1, 32767];
        let cited = format!("ESRI PE String = {bng}|");
        let crs = custom_crs(&keys(&cited), &[], &cited).unwrap();
        assert_ne!(crs, Crs::Epsg(27700));
        let prj = crate::raster::wkt::crs_from_wkt(bng).unwrap();
        let at = rustgis_geom::Coord::new(400_000.0, 300_000.0);
        let (a, b) = (crs.transformer().unwrap().to_lonlat(at).unwrap(), prj.transformer().unwrap().to_lonlat(at).unwrap());
        assert!((a.x - b.x).abs() < 1e-12 && (a.y - b.y).abs() < 1e-12, "{a:?} {b:?}");
        let plain = format!("ESRI PE String = {}|", bng.replace(",TOWGS84[375,-111,431,0,0,0,0]", ""));
        assert_eq!(custom_crs(&keys(&plain), &[], &plain).unwrap(), Crs::Epsg(27700));
        // Read from its `.prj` and written as keys, it keeps its shift: read back, it places
        // points as the `.prj` does.
        let (k, d, a) = terrio_keys(&prj.custom_def().unwrap()).unwrap();
        assert!(!k.chunks(4).any(|e| e == [3072, 0, 1, 27700]), "{k:?}");
        assert!(k.chunks(4).skip(1).any(|e| e[0] == 2062), "{k:?}");
        let back = custom_crs(&k, &d, &a).unwrap();
        let c = back.transformer().unwrap().to_lonlat(at).unwrap();
        assert!((c.x - b.x).abs() < 1e-9 && (c.y - b.y).abs() < 1e-9, "{c:?} {b:?}");
        // A shift of zero on another datum than WGS 84 (Lambert 93 on RGF93, as Esri writes it)
        // is written as user-defined keys with that shift, as GDAL 3.10.3 writes them too (no
        // 2154), and as RustGIS wrote it before: read back, it places points as the `.prj` does.
        let l93 = r#"PROJCS["RGF93_Lambert_93",GEOGCS["GCS_RGF_1993",DATUM["D_RGF_1993",SPHEROID["GRS_1980",6378137.0,298.257222101],TOWGS84[0,0,0,0,0,0,0]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Lambert_Conformal_Conic"],PARAMETER["False_Easting",700000.0],PARAMETER["False_Northing",6600000.0],PARAMETER["Central_Meridian",3.0],PARAMETER["Standard_Parallel_1",44.0],PARAMETER["Standard_Parallel_2",49.0],PARAMETER["Latitude_Of_Origin",46.5],UNIT["Meter",1.0]]"#;
        let l93 = crate::raster::wkt::crs_from_wkt(l93).unwrap();
        let (k, d, a) = terrio_keys(&l93.custom_def().unwrap_or_else(|| panic!("{l93:?}"))).unwrap();
        assert!(k.chunks(4).any(|e| e == [3072, 0, 1, 32767]), "{k:?}");
        let at = rustgis_geom::Coord::new(652_469.0, 6_862_035.0);
        let (back, prj) = (custom_crs(&k, &d, &a).unwrap().transformer().unwrap(), l93.transformer().unwrap());
        let (c, e) = (back.to_lonlat(at).unwrap(), prj.to_lonlat(at).unwrap());
        assert!((c.x - e.x).abs() < 1e-9 && (c.y - e.y).abs() < 1e-9, "{c:?} {e:?}");
    }

    /// MODIS's sinusoidal grid (coordinate transformation 24), as a MOD13Q1 tile's GeoKeys give
    /// it: h12v10, whose corner is at 60.925597° W, 10° S (pyproj 3.7.1).
    #[test]
    fn modis_sinusoidal_is_read_and_written_back() {
        let keys = [
            1u16, 1, 0, 15, 1024, 0, 1, 1, 1025, 0, 1, 1, 2048, 0, 1, 32767, 2050, 0, 1, 32767, 2054, 0, 1, 9102, 2056, 0, 1, 32767, 2057,
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
        assert!(p.contains("+proj=sinu") && crate::raster::crs::def_params(&p).is_some_and(|e| e.a == 6_371_007.181 && e.b == e.a), "{p}");
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
        // ESRI:54008's definition, on WGS 84, and one with a central meridian and false origin
        // away from 0 (ESRI:102011, Africa Sinusoidal, at 15°) go through the keys and back as the
        // same projection.
        keys_round_trip("+proj=sinu +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs");
        keys_round_trip("+proj=sinu +lon_0=15 +x_0=200000 +y_0=-100000 +datum=WGS84 +units=m +no_defs");
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
