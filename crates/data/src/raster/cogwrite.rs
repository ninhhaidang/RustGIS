//! Cloud-optimised GeoTIFF writing: a tiled, Deflate-compressed GeoTIFF with overviews (averaged,
//! or a pixel taken for classes), laid out so a reader can fetch the header with one request and
//! then any tile with another.
//!
//! The file starts with every image's directory (the full image first, then each overview), then
//! the tag arrays they point to, then the tiles of the smallest overview up to the full image's,
//! each image's tiles in row order. That is the layout GDAL's validator asks of a COG. The
//! directories' sizes depend only on the tile counts, so they are written first with empty tile
//! tables, and the tables are filled in once the tiles' places are known.
//!
//! Memory: the raster is read in place. A tile is gathered from the bands and compressed on its
//! own, and only a few tiles a thread are held before they are written, so no second copy of the
//! pixels is made, and each overview tile is made straight from the full image (a block of
//! 2ⁿ × 2ⁿ pixels), so no pyramid is kept. The price is time: every overview level reads the
//! whole full image again, so writing takes about as many passes over the pixels as there are
//! levels, where building each level from the one before would take about 1⅓ but hold a second
//! copy.
//!
//! Tiles are compressed with Deflate at its fastest level, after a predictor that makes neighbouring
//! samples alike: the floating-point one for floats, differences along the row for whole numbers.
//! Most of the time spent writing is compressing. Against Deflate's default level without a
//! predictor, this compresses an elevation model's tiles about four times as fast and a fifth
//! smaller, and an aerial photograph's three times as fast and a seventh smaller. Classes (a
//! palette's indexes, a few whole numbers) keep the default level and no predictor: their codes
//! don't change smoothly, so differences make them no smaller, and their long runs compress quickly
//! at any level, to half the size of the fastest.
//!
//! On the desktop the tiles are made on several threads ([`crate::threads::max`]) while one thread
//! writes them in their order, so the file is the same as one thread writes, byte for byte; in the
//! browser they are made one after another.
//!
//! Classic TIFF is used when the file can't pass 4 GiB even with every tile left uncompressed,
//! BigTIFF otherwise.
//!
//! The untiled GeoTIFF ([`super::geotiff::write`]) is written here too, by [`write_strips`]: one
//! image in strips of rows, with the same predictors, Deflate effort and threads, and no overviews.
//!
//! Alpha: a float file's alpha is opaque at 1, so the alpha of an integer raster written as floats
//! is divided by its opaque value (255 for bytes), as the untiled writer does.

use std::io::{Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;

use super::geotiff::{GeoKeys, fits_bytes, geo_keys};
use super::{ColorHint, Raster};
use crate::{DataError, Result};

/// Pixels along a tile's sides, in the full image and in every overview.
pub const TILE: u32 = 512;
/// The most images (full and overviews) written; 2³¹ pixels across needs far fewer.
const MAX_LEVELS: usize = 32;
/// Deflate effort for values that change smoothly: the fastest, which the predictor makes up for in
/// size (see the module's notes).
const DEFLATE_FAST: u8 = 1;
/// Deflate effort for classes, which take no predictor: the default.
const DEFLATE_CLASSES: u8 = 6;

fn err(e: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("COG: {e}"))
}

/// An error of the untiled writer ([`write_strips`]).
fn strip_err(e: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("GeoTIFF: {e}"))
}

/// Which TIFF flavour to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavour {
    /// Classic when the file can't pass 4 GiB, else BigTIFF.
    Auto,
    Classic,
    Big,
}

/// What was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CogInfo {
    pub big: bool,
    /// The full image's size, then each overview's.
    pub levels: Vec<(u32, u32)>,
    /// File size in bytes.
    pub bytes: u64,
}

/// The type of every sample in a file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Sample {
    Byte,
    Short,
    Float,
}

impl Sample {
    fn bytes(self) -> usize {
        match self {
            Sample::Byte => 1,
            Sample::Short => 2,
            Sample::Float => 4,
        }
    }

    /// BitsPerSample and SampleFormat (1 unsigned whole numbers, 3 floats).
    fn tiff_type(self) -> (u16, u16) {
        match self {
            Sample::Byte => (8, 1),
            Sample::Short => (16, 1),
            Sample::Float => (32, 3),
        }
    }
}

/// Whether a band holds classes: it names them, or holds a few distinct values. Their codes don't
/// change smoothly, so they take no predictor (see the module's notes).
pub(super) fn few_classes(b: &super::Band) -> bool {
    !b.classes.is_empty() || b.stats.categories.as_ref().is_some_and(|c| c.len() <= 64)
}

/// What goes into the file: which bands, as what.
struct Plan {
    bands: Vec<usize>,
    sample: Sample,
    rgb: bool,
    /// Which of `bands` is alpha, if one is.
    alpha: Option<usize>,
    /// The no-data value of whole-number samples.
    nodata_int: Option<u16>,
    /// The colour table of a palette raster, whose pixels are indexes.
    palette: Option<Vec<[u8; 4]>>,
    /// Classes (a palette's, or few whole numbers): overviews take a pixel, never an average.
    categorical: bool,
}

/// Whether band `b` holds whole numbers of a 16-bit type, and its no-data value (if any) is one.
fn fits_shorts(r: &Raster, b: usize) -> bool {
    let Some(band) = r.bands.get(b) else { return false };
    let s = &band.stats;
    let nodata_ok = r.nodata.is_none_or(|d| (0.0..=65_535.0).contains(&d) && d.fract() == 0.0);
    r.data_type == "UInt16" && nodata_ok && ((s.min >= 0.0 && s.max <= 65_535.0) || s.count == 0)
}

/// Chooses the file's samples like [`super::geotiff::write`] does, except for a colour cell with
/// no value under an opaque alpha: it is written see-through here (its alpha 0), not as floats
/// there. A colour raster's colour bands are written (as bytes when they are bytes, alpha kept),
/// otherwise `band` alone. 16-bit rasters stay 16-bit, and a grey byte raster is written as bytes
/// when no pixel needs a no-data value the bytes can't hold. Grey with alpha keeps both bands; a palette raster keeps its colour table.
fn plan(r: &Raster, band: usize) -> Result<Plan> {
    let int_nodata = |max: f64| r.nodata.filter(|d| (0.0..=max).contains(d) && d.fract() == 0.0).map(|d| d as u16);
    // The type of a set of bands: bytes, else 16-bit when that is what they are, else floats.
    let sample_of = |bands: &[usize]| {
        let bytes = bands.iter().all(|b| fits_bytes(r, *b));
        let shorts = bands.iter().all(|b| fits_shorts(r, *b));
        if r.data_type == "UInt16" && shorts {
            Some(Sample::Short)
        } else if bytes {
            Some(Sample::Byte)
        } else {
            None
        }
    };
    if matches!(r.color, ColorHint::Rgb { .. }) && r.bands.len() >= 3 {
        // A fourth band is alpha only when the raster says so (RGB and near-infrared isn't).
        let has_alpha = r.bands.len() >= 4 && matches!(r.color, ColorHint::Rgb { alpha: true });
        let rgbi = r.bands.len() == 4 && !has_alpha;
        let color_bands: Vec<usize> = (0..if rgbi { 4 } else { 3 }).collect();
        if let Some(sample) = sample_of(&color_bands) {
            let alpha = has_alpha && sample_of(&[0, 1, 2, 3]) == Some(sample);
            let max = if sample == Sample::Byte { 255.0 } else { 65_535.0 };
            // Pixels with no value need a way to say so: alpha (see-through, in tile_samples), or a
            // no-data value the samples hold. Otherwise they are written as floats, with NaN.
            let n = r.width as usize * r.height as usize;
            let holes = color_bands.iter().copied().any(|b| r.bands.get(b).is_some_and(|band| band.stats.count < n));
            // An alpha band that can't share the colour bands' type keeps its place through the
            // float plan below rather than being dropped.
            if alpha == has_alpha && (alpha || !holes || int_nodata(max).is_some()) {
                return Ok(Plan {
                    bands: (0..if alpha || rgbi { 4 } else { 3 }).collect(),
                    sample,
                    rgb: true,
                    alpha: alpha.then_some(3),
                    nodata_int: int_nodata(max),
                    palette: None,
                    categorical: false,
                });
            }
        }
        let alpha = has_alpha;
        let n = if alpha || rgbi { 4 } else { 3 };
        return Ok(Plan {
            bands: (0..n).collect(),
            sample: Sample::Float,
            rgb: true,
            alpha: alpha.then_some(3),
            nodata_int: None,
            palette: None,
            categorical: false,
        });
    }
    if matches!(r.color, ColorHint::GrayAlpha) && r.bands.len() >= 2 {
        let sample = sample_of(&[0, 1]).unwrap_or(Sample::Float);
        let max = if sample == Sample::Byte { 255.0 } else { 65_535.0 };
        let nodata_int = if sample == Sample::Float { None } else { int_nodata(max) };
        return Ok(Plan { bands: vec![0, 1], sample, rgb: false, alpha: Some(1), nodata_int, palette: None, categorical: false });
    }
    let b = r.bands.get(band).ok_or_else(|| err(format!("the raster has no band {}", band + 1)))?;
    if let ColorHint::Palette(colors) = &r.color {
        // Whole numbers (the band's categories), not just a byte type: an index of 1.5 isn't a class.
        let whole = b.stats.categories.is_some() || b.stats.count == 0;
        if band != 0 || !fits_bytes(r, 0) || !whole {
            return Err(err("a palette raster's indexes must be whole numbers from 0 to 255 to keep its colour table"));
        }
        // Pixels with no value (a warp's collar) take an index the raster doesn't use, as its
        // no-data value, so no class is invented for them.
        let n = r.width as usize * r.height as usize;
        let unused = || (0..=255u16).find(|v| !b.stats.categories.as_ref().is_some_and(|c| c.contains(&f64::from(*v))));
        let nodata_int = int_nodata(255.0).or_else(|| if b.stats.count < n { unused() } else { None });
        if nodata_int.is_none() && b.stats.count < n {
            return Err(err("the palette's 256 indexes are all used, so none is left for the cells with no value"));
        }
        return Ok(Plan {
            bands: vec![0],
            sample: Sample::Byte,
            rgb: false,
            alpha: None,
            nodata_int,
            palette: Some(colors.clone()),
            categorical: true,
        });
    }
    let all_valid = b.stats.count == r.width as usize * r.height as usize;
    let sample = match sample_of(&[band]) {
        Some(Sample::Byte) if r.data_type == "UInt8" && (int_nodata(255.0).is_some() || all_valid) => Sample::Byte,
        Some(Sample::Short) if int_nodata(65_535.0).is_some() || all_valid => Sample::Short,
        _ => Sample::Float,
    };
    let max = if sample == Sample::Byte { 255.0 } else { 65_535.0 };
    // A few distinct whole numbers are classes; photographs and rasters with a range are not.
    let categorical = !b.classes.is_empty() || (sample != Sample::Float && few_classes(b));
    Ok(Plan {
        bands: vec![band],
        sample,
        rgb: false,
        alpha: None,
        nodata_int: if sample == Sample::Float { None } else { int_nodata(max) },
        palette: None,
        categorical,
    })
}

/// The bands [`write`] keeps of `r` with `band` asked for, 0-based: the colour bands, grey and
/// alpha, or `band` alone; an error when it can't write `r`.
pub fn bands_written(r: &Raster, band: usize) -> Result<Vec<usize>> {
    plan(r, band).map(|p| p.bands)
}

/// One image of the file: the full one (factor 1) or an overview.
struct Level {
    w: u32,
    h: u32,
    /// Full-image pixels along a side of one of this image's pixels.
    factor: usize,
    across: u32,
    down: u32,
}

impl Level {
    fn new(w: u32, h: u32, factor: usize) -> Level {
        Level { w, h, factor, across: w.div_ceil(TILE), down: h.div_ceil(TILE) }
    }
    fn tiles(&self) -> usize {
        self.across as usize * self.down as usize
    }
}

/// The mean of the pixels of `plan`'s bands in the block of the full image that pixel (`ox`, `oy`)
/// of a level with `factor` covers (the middle pixel, for classes); NaN where none is valid. A band
/// with alpha is weighted by it, so see-through pixels don't darken the colours of an overview.
fn block(r: &Raster, plan: &Plan, factor: usize, ox: usize, oy: usize, out: &mut [f32]) {
    let (w, h) = (r.width as usize, r.height as usize);
    let (x0, y0) = (ox * factor, oy * factor);
    let (x1, y1) = ((x0 + factor).min(w), (y0 + factor).min(h));
    // With an alpha band, alpha says which pixels have no value: a colour channel that equals the
    // no-data value (a black red channel, with no-data 0) is a real colour.
    let valid = |v: f32| if plan.alpha.is_some() { v.is_finite() } else { r.is_valid(v) };
    if factor == 1 || plan.categorical {
        // Classes are never averaged: an overview takes the pixel in the middle of its block.
        let (x, y) = (x0 + (x1 - x0) / 2, y0 + (y1 - y0) / 2);
        for (o, &b) in out.iter_mut().zip(&plan.bands) {
            let v = r.bands.get(b).and_then(|band| band.data.get(y * w + x)).copied().unwrap_or(f32::NAN);
            *o = if valid(v) { v } else { f32::NAN };
        }
        return;
    }
    let alpha = plan.alpha.and_then(|a| r.bands.get(*plan.bands.get(a)?)).map(|b| b.data.as_slice());
    for (s, (o, &b)) in out.iter_mut().zip(&plan.bands).enumerate() {
        let Some(data) = r.bands.get(b).map(|b| &b.data) else {
            *o = f32::NAN;
            continue;
        };
        let weighted = alpha.filter(|_| plan.alpha != Some(s));
        let (mut sum, mut weight) = (0.0f64, 0.0f64);
        for y in y0..y1 {
            let (Some(row), arow) = (data.get(y * w + x0..y * w + x1), weighted.and_then(|a| a.get(y * w + x0..y * w + x1))) else {
                continue;
            };
            for (i, &v) in row.iter().enumerate() {
                if !valid(v) {
                    continue;
                }
                let k = match arow {
                    Some(a) => a.get(i).copied().filter(|a| valid(*a)).map_or(0.0, f64::from),
                    None => 1.0,
                };
                sum += f64::from(v) * k;
                weight += k;
            }
        }
        *o = if weight > 0.0 { (sum / weight) as f32 } else { f32::NAN };
    }
}

/// A tile's samples, uncompressed and interleaved. Pixels past the image's edge, and pixels with
/// no valid value, are the no-data value (0 for bytes, NaN for floats, when there is none).
fn tile_samples(r: &Raster, plan: &Plan, lv: &Level, tx: u32, ty: u32) -> Vec<u8> {
    let spp = plan.bands.len();
    let t = TILE as usize;
    let bps = plan.sample.bytes();
    let mut raw = Vec::with_capacity(t * t * spp * bps);
    let float_pad = r.nodata.map_or(f32::NAN, |d| d as f32);
    let int_pad = plan.nodata_int.unwrap_or(0);
    let opaque = super::alpha_max(&r.data_type);
    let mut px = vec![f32::NAN; spp];
    for y in 0..t {
        let oy = ty as usize * t + y;
        for x in 0..t {
            let ox = tx as usize * t + x;
            if oy < lv.h as usize && ox < lv.w as usize {
                block(r, plan, lv.factor, ox, oy, &mut px);
            } else {
                px.fill(f32::NAN);
            }
            // A colour with no value is see-through, never a black pixel that reads as real.
            if let Some(a) = plan.alpha
                && px.iter().enumerate().any(|(i, v)| i != a && v.is_nan())
                && let Some(alpha) = px.get_mut(a)
            {
                *alpha = 0.0;
            }
            // A float file's alpha is opaque at 1: an integer raster's is divided by its opaque value.
            // (No no-data tag goes with an alpha band, so none of its cells is a no-data one.)
            if plan.sample == Sample::Float
                && let Some(alpha) = plan.alpha.and_then(|a| px.get_mut(a))
            {
                *alpha /= opaque;
            }
            for v in &px {
                match plan.sample {
                    Sample::Byte => raw.push(if v.is_nan() { int_pad as u8 } else { v.round().clamp(0.0, 255.0) as u8 }),
                    Sample::Short => {
                        let n = if v.is_nan() { int_pad } else { v.round().clamp(0.0, 65_535.0) as u16 };
                        raw.extend_from_slice(&n.to_le_bytes());
                    }
                    Sample::Float => raw.extend_from_slice(&(if v.is_nan() { float_pad } else { *v }).to_le_bytes()),
                }
            }
        }
    }
    raw
}

/// The TIFF predictor `plan`'s tiles are written with: none (1) for classes, differences along the
/// row (2) for whole numbers, the floating-point one (3) for floats.
fn predictor(plan: &Plan) -> u16 {
    predictor_of(plan.sample, plan.categorical)
}

/// The TIFF predictor for samples of type `sample`: none (1) for classes, differences along the row
/// (2) for whole numbers, the floating-point one (3) for floats.
fn predictor_of(sample: Sample, classes: bool) -> u16 {
    match sample {
        _ if classes => 1,
        Sample::Byte | Sample::Short => 2,
        Sample::Float => 3,
    }
}

/// Applies `plan`'s predictor to a tile's samples (as [`tile_samples`] makes them); `row` is
/// scratch space for the floating-point predictor's reordering.
fn predict(raw: &mut [u8], plan: &Plan, row: &mut Vec<u8>) {
    let line = TILE as usize * plan.bands.len() * plan.sample.bytes();
    predict_rows(raw, line, plan.bands.len(), plan.sample, predictor(plan), row);
}

/// Applies `predictor` to rows of `line` bytes each, `spp` samples of type `sample` to a pixel, one
/// row at a time; `row` is scratch space for the floating-point predictor's reordering.
fn predict_rows(raw: &mut [u8], line: usize, spp: usize, sample: Sample, predictor: u16, row: &mut Vec<u8>) {
    let size = sample.bytes();
    // An image with no columns has no rows to predict (and `chunks_exact_mut` takes no zero).
    if line == 0 {
        return;
    }
    for line in raw.chunks_exact_mut(line) {
        match (predictor, sample) {
            (2, Sample::Byte) => differ(line, spp),
            (2, Sample::Short) => {
                // Each 16-bit sample less the one a pixel before it, wrapping; last first, so each
                // takes away a value not yet changed.
                for i in (spp..line.len() / 2).rev() {
                    let word =
                        |l: &[u8], j: usize| l.get(2 * j..2 * j + 2).map_or(0, |b| u16::from_le_bytes(b.try_into().unwrap_or([0; 2])));
                    let v = word(line, i).wrapping_sub(word(line, i - spp));
                    if let Some(b) = line.get_mut(2 * i..2 * i + 2) {
                        b.copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
            (3, _) => {
                // The row's bytes regrouped by significance, the most significant byte of every
                // sample first, then differenced (TIFF Technical Note 3).
                let n = line.len() / size;
                row.clear();
                row.resize(line.len(), 0);
                for (i, sample) in line.chunks_exact(size).enumerate() {
                    // Samples are little-endian: the last byte is the most significant.
                    for (k, b) in sample.iter().rev().enumerate() {
                        if let Some(o) = row.get_mut(k * n + i) {
                            *o = *b;
                        }
                    }
                }
                line.copy_from_slice(row);
                differ(line, spp);
            }
            _ => {}
        }
    }
}

/// Each byte of `line` less the one `stride` bytes before it, wrapping.
fn differ(line: &mut [u8], stride: usize) {
    for i in (stride..line.len()).rev() {
        let prev = line.get(i - stride).copied().unwrap_or(0);
        if let Some(b) = line.get_mut(i) {
            *b = b.wrapping_sub(prev);
        }
    }
}

// TIFF field types.
const ASCII: u16 = 2;
const SHORT: u16 = 3;
const LONG: u16 = 4;
const DOUBLE: u16 = 12;
const LONG8: u16 = 16;

/// A directory entry; `patch` marks a tile (or strip) table, filled in after they are written.
pub(super) struct Entry {
    tag: u16,
    ty: u16,
    count: u64,
    data: Vec<u8>,
    patch: Option<bool>,
}

/// An entry of 16-bit values.
pub(super) fn shorts(tag: u16, v: &[u16]) -> Entry {
    Entry { tag, ty: SHORT, count: v.len() as u64, data: v.iter().flat_map(|x| x.to_le_bytes()).collect(), patch: None }
}

/// An entry of one 32-bit value.
pub(super) fn long(tag: u16, v: u32) -> Entry {
    Entry { tag, ty: LONG, count: 1, data: v.to_le_bytes().to_vec(), patch: None }
}

/// An entry of 64-bit floats.
pub(super) fn doubles(tag: u16, v: &[f64]) -> Entry {
    Entry { tag, ty: DOUBLE, count: v.len() as u64, data: v.iter().flat_map(|x| x.to_le_bytes()).collect(), patch: None }
}

/// An entry of text, NUL-terminated as TIFF ASCII is.
pub(super) fn ascii(tag: u16, s: &str) -> Entry {
    let mut data = s.as_bytes().to_vec();
    data.push(0);
    Entry { tag, ty: ASCII, count: data.len() as u64, data, patch: None }
}

/// A tile table of `n` zeros (offsets, or byte counts), to patch.
fn table(tag: u16, n: usize, big: bool, counts: bool) -> Entry {
    let width = if big { 8 } else { 4 };
    Entry { tag, ty: if big { LONG8 } else { LONG }, count: n as u64, data: vec![0; n * width], patch: Some(counts) }
}

/// The directory of image `i` (0 is the full one), entries in tag order. Georeferencing is on the
/// full image only, as GDAL writes it.
fn entries(r: &Raster, plan: &Plan, lv: &Level, first: bool, geo: &GeoKeys, big: bool) -> Vec<Entry> {
    let spp = plan.bands.len();
    let (bits, format) = plan.sample.tiff_type();
    let mut e = Vec::new();
    if !first {
        e.push(long(254, 1));
    }
    e.push(long(256, lv.w));
    e.push(long(257, lv.h));
    e.push(shorts(258, &vec![bits; spp]));
    e.push(shorts(259, &[8]));
    e.push(shorts(
        262,
        &[if plan.palette.is_some() {
            3
        } else if plan.rgb {
            2
        } else {
            1
        }],
    ));
    e.push(shorts(277, &[spp as u16]));
    e.push(shorts(284, &[1]));
    e.push(shorts(317, &[predictor(plan)]));
    if let Some(colors) = &plan.palette {
        // Reds, then greens, then blues, 256 of each, as 16-bit intensities.
        let map: Vec<u16> = (0..3).flat_map(|c| (0..256).map(move |i| colors.get(i).map_or(0, |rgba| u16::from(rgba[c]) * 257))).collect();
        e.push(shorts(320, &map));
    }
    e.push(long(322, TILE));
    e.push(long(323, TILE));
    e.push(table(324, lv.tiles(), big, false));
    e.push(table(325, lv.tiles(), big, true));
    // Samples past the colour or grey ones: alpha, or nothing said of them (an RGB and
    // near-infrared image's fourth, a stream's further bands).
    let base = if plan.rgb { 3 } else { 1 };
    if spp > base {
        let extra: Vec<u16> = (base..spp).map(|i| if plan.alpha == Some(i) { 2 } else { 0 }).collect();
        e.push(shorts(338, &extra));
    }
    e.push(shorts(339, &vec![format; spp]));
    if first {
        let n = r.width as usize * r.height as usize;
        let nodata = match (plan.sample, r.nodata) {
            // Alpha says which pixels have no value; a no-data tag would make readers that apply it
            // per channel take a real colour (a black red channel) for an empty one.
            _ if plan.alpha.is_some() => None,
            (Sample::Byte | Sample::Short, _) => plan.nodata_int.map(|b| b.to_string()),
            (Sample::Float, Some(d)) => Some(d.to_string()),
            // NaN marks the pixels with no value; say so for readers that don't assume it.
            (Sample::Float, None) => {
                plan.bands.iter().any(|b| r.bands.get(*b).is_some_and(|b| b.stats.count < n)).then(|| "nan".to_string())
            }
        };
        e.extend(geo_entries(r, geo, &plan.bands, nodata));
    }
    e
}

/// The full image's tags past its layout, in tag order: who holds the rights, where it lies, the
/// names of the bands written as samples (`written`) and the first one's class names, and
/// `nodata`, the value that marks pixels with none.
pub(super) fn geo_entries(r: &Raster, geo: &GeoKeys, written: &[usize], nodata: Option<String>) -> Vec<Entry> {
    let mut e = Vec::new();
    if let Some(c) = r.copyright.as_deref().map(super::tag_text).filter(|c| !c.is_empty()) {
        e.push(ascii(33432, &c));
    }
    let gt = r.transform;
    e.push(doubles(33550, &[gt.pixel_width.abs(), gt.pixel_height.abs(), 0.0]));
    e.push(doubles(33922, &[0.0, 0.0, 0.0, gt.origin_x, gt.origin_y, 0.0]));
    e.push(shorts(34735, &geo.keys));
    if let Some(d) = &geo.custom {
        if !d.doubles.is_empty() {
            e.push(doubles(34736, &d.doubles));
        }
        if !d.ascii.is_empty() {
            e.push(ascii(34737, &d.ascii));
        }
    }
    // The written bands' names and the first one's class names, so a reopened window keeps them (a
    // band keeping its number has no name to write).
    if let Some(xml) = super::gdal_metadata_of(r, written) {
        e.push(ascii(42112, &xml));
    }
    if let Some(nd) = nodata {
        e.push(ascii(42113, &nd));
    }
    e
}

/// Where a tile table's values go in the file.
struct Patch {
    level: usize,
    counts: bool,
    at: u64,
}

/// The file up to its first tile, and where the tile tables are.
struct Head {
    bytes: Vec<u8>,
    patches: Vec<Patch>,
}

/// Lays out the header, every directory (chained, one after another), then their out-of-line values.
fn build_head(levels: &[Vec<Entry>], big: bool) -> Head {
    let hdr = if big { 16u64 } else { 8 };
    let inline = if big { 8 } else { 4 };
    let ifd_len = |n: usize| -> u64 { if big { 8 + 20 * n as u64 + 8 } else { 2 + 12 * n as u64 + 4 } };
    let mut starts = Vec::with_capacity(levels.len());
    let mut cursor = hdr;
    for l in levels {
        starts.push(cursor);
        cursor += ifd_len(l.len());
    }
    let ext_base = cursor;
    let mut out = Vec::new();
    if big {
        out.extend_from_slice(&[b'I', b'I', 43, 0, 8, 0, 0, 0]);
        out.extend_from_slice(&starts.first().copied().unwrap_or(0).to_le_bytes());
    } else {
        out.extend_from_slice(&[b'I', b'I', 42, 0]);
        out.extend_from_slice(&(starts.first().copied().unwrap_or(0) as u32).to_le_bytes());
    }
    let mut ext: Vec<u8> = Vec::new();
    let mut patches = Vec::new();
    for (i, l) in levels.iter().enumerate() {
        if big {
            out.extend_from_slice(&(l.len() as u64).to_le_bytes());
        } else {
            out.extend_from_slice(&(l.len() as u16).to_le_bytes());
        }
        for e in l {
            out.extend_from_slice(&e.tag.to_le_bytes());
            out.extend_from_slice(&e.ty.to_le_bytes());
            if big {
                out.extend_from_slice(&e.count.to_le_bytes());
            } else {
                out.extend_from_slice(&(e.count as u32).to_le_bytes());
            }
            let value_at = out.len() as u64;
            let at = if e.data.len() <= inline {
                out.extend_from_slice(&e.data);
                out.resize(out.len() + (inline - e.data.len()), 0);
                value_at
            } else {
                if ext.len() % 2 == 1 {
                    ext.push(0);
                }
                let off = ext_base + ext.len() as u64;
                ext.extend_from_slice(&e.data);
                if big {
                    out.extend_from_slice(&off.to_le_bytes());
                } else {
                    out.extend_from_slice(&(off as u32).to_le_bytes());
                }
                off
            };
            if let Some(counts) = e.patch {
                patches.push(Patch { level: i, counts, at });
            }
        }
        let next = starts.get(i + 1).copied().unwrap_or(0);
        if big {
            out.extend_from_slice(&next.to_le_bytes());
        } else {
            out.extend_from_slice(&(next as u32).to_le_bytes());
        }
    }
    out.extend_from_slice(&ext);
    // Tiles start on an 8-byte boundary.
    out.resize(out.len().next_multiple_of(8), 0);
    Head { bytes: out, patches }
}

/// Writes a raster as a cloud-optimised GeoTIFF into `out` (which must be empty: offsets are from
/// its start). `band` (0-based) is the band of a raster that isn't a colour one.
pub fn write_to<W: Write + Seek>(r: &Raster, band: usize, flavour: Flavour, out: &mut W) -> Result<CogInfo> {
    // A scan's place is only where it was laid on the view: not coordinates to write into a file.
    r.require_coordinates().map_err(err)?;
    let gt = r.transform;
    if gt.pixel_height > 0.0 || gt.pixel_width < 0.0 {
        return Err(err("only north-up rasters can be exported for now"));
    }
    let plan = plan(r, band)?;
    let geo = geo_keys(r)?;
    let spp = plan.bands.len();
    let bps = plan.sample.bytes();

    let mut levels = vec![Level::new(r.width, r.height, 1)];
    while let Some(last) = levels.last().filter(|l| l.w.max(l.h) > TILE) {
        if levels.len() >= MAX_LEVELS {
            return Err(err("too many overviews"));
        }
        let (w, h, f) = (last.w.div_ceil(2), last.h.div_ceil(2), last.factor * 2);
        levels.push(Level::new(w, h, f));
    }

    // The worst a file can come to: every tile uncompressed (Deflate's expansion is under 0.1%),
    // plus the headers and tile tables.
    let tiles: usize = levels.iter().map(Level::tiles).sum();
    let raw = (tiles as u64).saturating_mul((TILE as u64 * TILE as u64) * (spp * bps) as u64);
    let worst = raw.saturating_add(raw / 512).saturating_add(tiles as u64 * 64).saturating_add(1 << 20);
    let big = match flavour {
        Flavour::Auto => worst >= u64::from(u32::MAX),
        Flavour::Classic => false,
        Flavour::Big => true,
    };

    let ifds: Vec<Vec<Entry>> = levels.iter().enumerate().map(|(i, lv)| entries(r, &plan, lv, i == 0, &geo, big)).collect();
    let head = build_head(&ifds, big);
    out.seek(SeekFrom::Start(0)).map_err(err)?;
    out.write_all(&head.bytes).map_err(err)?;
    let mut pos = head.bytes.len() as u64;

    // Smallest overview first, the full image last; each in row order.
    let order: Vec<(usize, u32, u32)> = levels
        .iter()
        .enumerate()
        .rev()
        .flat_map(|(i, lv)| (0..lv.down).flat_map(move |ty| (0..lv.across).map(move |tx| (i, tx, ty))))
        .collect();
    let mut places: Vec<Vec<(u64, u64)>> = vec![Vec::new(); levels.len()];
    let effort = if plan.categorical { DEFLATE_CLASSES } else { DEFLATE_FAST };
    let tile_bytes = (TILE as usize * TILE as usize).saturating_mul(spp * bps);
    in_order(
        order.len(),
        tile_bytes,
        err,
        Vec::new,
        |row, k| {
            let (i, tx, ty) = order.get(k).copied().ok_or_else(|| err("a tile past the image"))?;
            let lv = levels.get(i).ok_or_else(|| err("a tile past the image"))?;
            let mut raw = tile_samples(r, &plan, lv, tx, ty);
            predict(&mut raw, &plan, row);
            Ok(miniz_oxide::deflate::compress_to_vec_zlib(&raw, effort))
        },
        |k, zipped| {
            if !big && pos + zipped.len() as u64 > u64::from(u32::MAX) {
                return Err(err("the file is too large for classic TIFF"));
            }
            out.write_all(&zipped).map_err(err)?;
            if let Some(p) = order.get(k).and_then(|(i, _, _)| places.get_mut(*i)) {
                p.push((pos, zipped.len() as u64));
            }
            pos += zipped.len() as u64;
            Ok(())
        },
    )?;

    fill_tables(out, &head.patches, &places, big, pos, err)?;
    Ok(CogInfo { big, levels: levels.iter().map(|l| (l.w, l.h)).collect(), bytes: pos })
}

/// Fills in the tile (or strip) tables of `patches` with where each piece of image `level` went in
/// the file and how long it is (`places`), then moves `out` back to the file's end, `end`.
fn fill_tables<W: Write + Seek>(
    out: &mut W,
    patches: &[Patch],
    places: &[Vec<(u64, u64)>],
    big: bool,
    end: u64,
    err: fn(String) -> DataError,
) -> Result<()> {
    for p in patches {
        let Some(pieces) = places.get(p.level) else { continue };
        let mut bytes = Vec::with_capacity(pieces.len() * 8);
        for (off, len) in pieces {
            let v = if p.counts { *len } else { *off };
            if big {
                bytes.extend_from_slice(&v.to_le_bytes());
            } else {
                bytes.extend_from_slice(&u32::try_from(v).map_err(|e| err(e.to_string()))?.to_le_bytes());
            }
        }
        out.seek(SeekFrom::Start(p.at)).map_err(|e| err(e.to_string()))?;
        out.write_all(&bytes).map_err(|e| err(e.to_string()))?;
    }
    out.seek(SeekFrom::Start(end)).map_err(|e| err(e.to_string()))?;
    Ok(())
}

/// An untiled image for [`write_strips`]: how its pixels are laid out, and its other tags (from
/// [`geo_entries`], and any more), in any order.
pub(super) struct Strips {
    pub w: u32,
    pub h: u32,
    /// Samples to a pixel, interleaved.
    pub spp: u16,
    pub sample: Sample,
    /// PhotometricInterpretation: 1 for grey, 2 for RGB.
    pub photometric: u16,
    /// The ExtraSamples values of the samples past the grey or colour ones (none to leave it out).
    pub extra: Vec<u16>,
    /// Classes: no predictor, and the default Deflate effort (see the module's notes).
    pub classes: bool,
    pub tags: Vec<Entry>,
}

/// Uncompressed bytes a strip holds at most, as whole rows (one row when a row is longer): enough
/// for Deflate to find what repeats, small enough to hold one strip at a time.
const STRIP_BYTES: u64 = 1 << 20;

/// Writes an untiled TIFF of one image into `out` (which must be empty: offsets are from its
/// start): its directory and tags, then its strips, each made by `rows` (which appends the
/// samples of a range of rows, interleaved and little-endian), run through the predictor and
/// compressed with Deflate, on several threads on the desktop ([`in_order`]). Classic TIFF when
/// the file can't pass 4 GiB, BigTIFF otherwise. It counts the strips as its progress, and a
/// cancelled run stops it within the few strips being made.
pub(super) fn write_strips<W: Write + Seek>(
    img: Strips,
    out: &mut W,
    rows: impl Fn(Range<usize>, &mut Vec<u8>) -> Result<()> + Sync,
) -> Result<()> {
    if img.w == 0 || img.h == 0 {
        return Err(strip_err("the raster has no pixels to write"));
    }
    let spp = usize::from(img.spp);
    let line = img.w as usize * spp * img.sample.bytes();
    let per_strip = (STRIP_BYTES / (line as u64).max(1)).clamp(1, u64::from(img.h.max(1))) as u32;
    let strips = img.h.div_ceil(per_strip) as usize;
    let predictor = predictor_of(img.sample, img.classes);

    // The worst a file can come to: every strip uncompressed (Deflate's expansion is under 0.1%),
    // plus the headers, the strip tables and the other tags.
    let raw = (line as u64).saturating_mul(u64::from(img.h));
    let tags: u64 = img.tags.iter().map(|e| e.data.len() as u64).sum();
    let worst = raw.saturating_add(raw / 512).saturating_add(strips as u64 * 64).saturating_add(tags).saturating_add(1 << 20);
    let big = worst >= u64::from(u32::MAX);

    let (bits, format) = img.sample.tiff_type();
    let mut e = vec![
        long(256, img.w),
        long(257, img.h),
        shorts(258, &vec![bits; spp]),
        shorts(259, &[8]),
        shorts(262, &[img.photometric]),
        table(273, strips, big, false),
        shorts(277, &[img.spp]),
        long(278, per_strip),
        table(279, strips, big, true),
        shorts(284, &[1]),
        shorts(317, &[predictor]),
        shorts(339, &vec![format; spp]),
    ];
    if !img.extra.is_empty() {
        e.push(shorts(338, &img.extra));
    }
    e.extend(img.tags);
    // A directory's entries go in tag order.
    e.sort_by_key(|e| e.tag);
    let head = build_head(&[e], big);
    out.seek(SeekFrom::Start(0)).map_err(strip_err)?;
    out.write_all(&head.bytes).map_err(strip_err)?;
    let mut pos = head.bytes.len() as u64;

    let effort = if img.classes { DEFLATE_CLASSES } else { DEFLATE_FAST };
    let mut places = vec![Vec::with_capacity(strips)];
    let (h, per_strip) = (img.h as usize, per_strip as usize);
    in_order(
        strips,
        line.saturating_mul(per_strip),
        strip_err,
        || (Vec::new(), Vec::new()),
        |(raw, row), k| {
            let y = k * per_strip;
            raw.clear();
            rows(y..(y + per_strip).min(h), raw)?;
            predict_rows(raw, line, spp, img.sample, predictor, row);
            Ok(miniz_oxide::deflate::compress_to_vec_zlib(raw, effort))
        },
        |_, zipped| {
            if !big && pos + zipped.len() as u64 > u64::from(u32::MAX) {
                return Err(strip_err("the file is too large for classic TIFF"));
            }
            out.write_all(&zipped).map_err(strip_err)?;
            if let Some(p) = places.first_mut() {
                p.push((pos, zipped.len() as u64));
            }
            pos += zipped.len() as u64;
            Ok(())
        },
    )?;
    fill_tables(out, &head.patches, &places, big, pos, strip_err)
}

/// Makes `count` pieces of a file (tiles or strips, each about `bytes` uncompressed) with `make`
/// and hands each to `put` in their order, with its index. `make` gets the scratch its thread
/// made with `scratch` once; `err` words a failure of the threads. Every piece stands alone, so
/// on the desktop, with more than one piece, they are made on several threads
/// ([`crate::threads::max`], fewer when [`crate::memory::thread_scratch`] can't hold their pieces)
/// while this thread writes them: the same file as one thread makes, byte for byte. In the browser
/// they are made one after another.
///
/// It counts the pieces as its progress, and a cancelled run stops it within the few pieces being
/// made ([`AHEAD`] a thread). The first error in the pieces' order comes back, as on one thread.
fn in_order<S>(
    count: usize,
    bytes: usize,
    err: fn(String) -> DataError,
    scratch: impl Fn() -> S + Sync,
    make: impl Fn(&mut S, usize) -> Result<Vec<u8>> + Sync,
    put: impl FnMut(usize, Vec<u8>) -> Result<()>,
) -> Result<()> {
    crate::progress::begin(count);
    // A piece being made holds about twice its bytes (its samples and their compressed copy), and
    // each waiting to be written up to its bytes again.
    let affordable = 1 + crate::memory::thread_scratch() / bytes.saturating_mul(2 + AHEAD).max(1);
    let threads = crate::threads::for_items(count).min(affordable);
    if threads > 1 {
        return in_order_parallel(count, threads, err, scratch, make, put);
    }
    one_by_one(count, err, scratch, make, put)?;
    // A cancel that came while the last piece was made stops the write too.
    crate::progress::check()
}

/// [`in_order`] on this thread alone: each piece made, then written, before the next.
fn one_by_one<S>(
    count: usize,
    err: fn(String) -> DataError,
    scratch: impl Fn() -> S,
    make: impl Fn(&mut S, usize) -> Result<Vec<u8>>,
    mut put: impl FnMut(usize, Vec<u8>) -> Result<()>,
) -> Result<()> {
    let mut s = scratch();
    for k in 0..count {
        crate::progress::step(1)?;
        put(k, caught(&make, &mut s, k, err).0?)?;
    }
    Ok(())
}

/// Piece `k` from `make`, a panic in it as an error, so the write fails the same way on any number
/// of threads; and whether it panicked, after which `s` may be half changed.
fn caught<S>(
    make: impl Fn(&mut S, usize) -> Result<Vec<u8>>,
    s: &mut S,
    k: usize,
    err: fn(String) -> DataError,
) -> (Result<Vec<u8>>, bool) {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| make(s, k))) {
        Ok(made) => (made, false),
        Err(_) => (Err(err("a piece of the image couldn't be made".into())), true),
    }
}

/// The pieces a thread of [`in_order`] may make ahead of the one being written: enough that the
/// threads rarely wait for a slow piece, few enough that the pieces waiting stay small.
const AHEAD: usize = 2;

/// [`in_order`] on `threads` threads besides this one, which writes. A thread takes the next piece
/// unless it is more than [`AHEAD`] a thread past the one to write next, so a slow piece holds up
/// only that many. A piece's panic comes back as an error in its place. A thread the OS refuses
/// costs speed, not the write; when it refuses all of them, this thread makes the pieces itself.
fn in_order_parallel<S>(
    count: usize,
    threads: usize,
    err: fn(String) -> DataError,
    scratch: impl Fn() -> S + Sync,
    make: impl Fn(&mut S, usize) -> Result<Vec<u8>> + Sync,
    mut put: impl FnMut(usize, Vec<u8>) -> Result<()>,
) -> Result<()> {
    use std::collections::BTreeMap;
    use std::sync::{Condvar, Mutex, mpsc};

    /// The next piece to take, the next to write, and whether to stop taking pieces.
    struct Turn {
        next: usize,
        written: usize,
        stop: bool,
    }
    let ahead = threads.saturating_mul(AHEAD);
    let turn = Mutex::new(Turn { next: 0, written: 0, stop: false });
    let moved = Condvar::new();
    let failed = || err("a piece of the image couldn't be made".into());
    // Takes the next piece once it is no more than `ahead` past the one to write; None to stop.
    let take = || -> Option<usize> {
        let mut t = turn.lock().ok()?;
        while !t.stop && t.next < count && t.next >= t.written.saturating_add(ahead) {
            t = moved.wait(t).ok()?;
        }
        if t.stop || t.next >= count {
            return None;
        }
        t.next += 1;
        Some(t.next - 1)
    };
    /// Stops the threads when dropped, however the writing ends: done, an error, or a panic in
    /// `put`, which would otherwise leave them waiting for the writer for good, and the scope
    /// waiting for them.
    struct Stop<'a>(&'a Mutex<Turn>, &'a Condvar);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            if let Ok(mut t) = self.0.lock() {
                t.stop = true;
            }
            self.1.notify_all();
        }
    }

    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<(usize, Result<Vec<u8>>)>();
        let work = |tx: mpsc::Sender<(usize, Result<Vec<u8>>)>| {
            let mut s = scratch();
            while let Some(k) = take() {
                let (made, panicked) = caught(&make, &mut s, k, err);
                // A thread whose piece panicked stops: its scratch may be half changed.
                if tx.send((k, made)).is_err() || panicked {
                    break;
                }
            }
        };
        let started = (0..threads)
            .filter_map(|_| {
                let tx = tx.clone();
                std::thread::Builder::new().spawn_scoped(scope, move || work(tx)).ok()
            })
            .count();
        drop(tx);
        // The threads finish the pieces they are making and take no more; the scope waits for them.
        let _stop = Stop(&turn, &moved);
        (|| -> Result<()> {
            if started == 0 {
                return one_by_one(count, err, &scratch, &make, &mut put);
            }
            // Pieces made before the ones ahead of them, by their index.
            let mut waiting = BTreeMap::new();
            for k in 0..count {
                let piece = loop {
                    if let Some(p) = waiting.remove(&k) {
                        break p;
                    }
                    // Every thread gone before piece `k` came: one panicked outside a piece.
                    let (i, p) = rx.recv().map_err(|_| failed())?;
                    waiting.insert(i, p);
                };
                crate::progress::step(1)?;
                put(k, piece?)?;
                if let Ok(mut t) = turn.lock() {
                    t.written = k + 1;
                }
                moved.notify_all();
            }
            Ok(())
        })()
    })?;
    // A cancel that came while the last piece was made stops the write too.
    crate::progress::check()
}

/// The bands [`write`] and [`write_file`] write of `r` given `band`, from the same plan.
pub fn written_bands(r: &Raster, band: usize) -> Result<Vec<usize>> {
    Ok(plan(r, band)?.bands)
}

/// A raster as the bytes of a cloud-optimised GeoTIFF (for the browser, which downloads them).
pub fn write(r: &Raster, band: usize) -> Result<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    write_to(r, band, Flavour::Auto, &mut out)?;
    Ok(out.into_inner())
}

/// Writes a cloud-optimised GeoTIFF to `path` through a temporary file beside it, synced and
/// renamed into place, so an interrupted write never truncates an existing file.
pub fn write_file(r: &Raster, band: usize, path: &Path) -> Result<CogInfo> {
    let tmp = crate::io::temp_beside(path)?;
    let res = (|| -> Result<CogInfo> {
        let f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
        let info = write_to(r, band, Flavour::Auto, &mut w)?;
        let f = w.into_inner().map_err(|e| err(e.error()))?;
        f.sync_all()?;
        drop(f);
        // A cancel that came while it was written leaves the file there before as it was. This is
        // the last look: the rename commits the file, and a Cancel after it finds it written.
        crate::progress::check()?;
        std::fs::rename(&tmp, path)?;
        crate::io::sync_folder(path);
        Ok(info)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// [`write_file`]'s file, written and synced beside `path` but not yet in its place: the caller
/// installs it ([`crate::io::StagedWrite::commit`]) once nothing can cancel the result any more,
/// and dropping it leaves `path` as it was.
pub fn write_staged(r: &Raster, band: usize, path: &Path) -> Result<(CogInfo, crate::io::StagedWrite)> {
    let (staged, info) = crate::io::stage_streamed(path, |w| write_to(r, band, Flavour::Auto, w))?;
    // The last look while it is written: a Cancel that came meanwhile leaves `path` as it was.
    crate::progress::check()?;
    Ok((info, staged))
}

/// A tiled GeoTIFF of 32-bit floats, one band or several, written a row of tiles at a time, for a
/// raster made in pieces too large to hold whole (#2865). The caller gives the full image's rows,
/// top to bottom, in as many calls as it likes ([`TileStream::put`], [`TileStream::put_bands`]);
/// each full row of tiles is compressed and written as it is complete, and the rows of the
/// overviews are made from it as they go, each level averaging two by two the pixels of the one
/// before (the pixels with a value among the four, as [`write_to`] averages, but from the level
/// before rather than the full image, and each band by itself). Its layout is a tiled GeoTIFF's,
/// every level's directory first: not a cloud-optimised one, whose tiles go smallest level first,
/// but one this computer's reader reads in place.
///
/// Memory: at most two rows of tiles of each level wait to be written, about
/// `width × TILE × 4 × 2 × 4 / 3` bytes a band, and a few compressed tiles a thread.
pub struct TileStream<'a, W: Write + Seek> {
    out: &'a mut W,
    levels: Vec<Level>,
    /// What the file holds: its bands, their type, colour and alpha.
    plan: Plan,
    /// Each level's rows given and not yet written as a row of tiles, a band each, its width each.
    pending: Vec<Vec<Vec<f32>>>,
    /// How many rows of each level have been given.
    given: Vec<u64>,
    places: Vec<Vec<(u64, u64)>>,
    patches: Vec<Patch>,
    big: bool,
    pos: u64,
    /// The value of a pixel with none as the file holds it, written for pixels past the image's
    /// edge too.
    nodata: f32,
    /// The value of a pixel with none as it is given (the template's), which the overviews are
    /// made with: the file's, unless alpha says which pixels have one ([`Holding`]).
    given_nodata: f32,
    /// The alpha band, which says which pixels have a value, and the value it is opaque at (an
    /// integer template's, which a float file holds divided by it, opaque at 1, as [`write_to`]
    /// writes it).
    alpha_scale: Option<(usize, f32)>,
    /// Whole numbers 1 to 255 written as bytes, 0 for no value ([`Samples::Hillshade`]); floats
    /// otherwise.
    bytes: bool,
    /// Classes, whose overviews take a pixel ([`Samples::Classes`]).
    classes: bool,
    /// The level kept as it is written ([`TileStream::keeping`]), and its rows so far, a band each.
    kept: Option<(usize, Vec<Vec<f32>>)>,
    /// What the kept level is made into: the template's name, bands' names, place, colour and
    /// no-data.
    template: Raster,
}

/// What a [`TileStream`]'s bands hold, which decides how they are stored and how their overviews
/// are made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Samples {
    /// Measures, as 32-bit floats; an overview pixel is the mean of the four it covers.
    Floats,
    /// Classes (a remap's, a threshold's), as 32-bit floats; an overview pixel takes one of the
    /// four it covers, never an average no class has, as [`write_to`] makes a classes' overview.
    Classes,
    /// A hillshade's whole numbers 1 to 255, as bytes with 0 for no value; averaged, which never
    /// gives 0 (no value) from pixels that have one. One band only.
    Hillshade,
}

impl<'a, W: Write + Seek> TileStream<'a, W> {
    /// A stream of a `width` × `height` raster of `samples` into `out` (which must be empty:
    /// offsets are from its start), placed, named and with the no-data value of `template`, and
    /// with as many bands as it has (their names, its colour: red, green and blue for an RGB
    /// template of three or four bands, the fourth alpha when it says so; grey and alpha; else
    /// grey, further bands saying nothing of what they are), its transform's corner and pixel
    /// size, its system and rights holder; its own pixels and size aren't written. A template
    /// without a no-data value marks pixels with none NaN; a hillshade's no-data is 0.
    pub fn new(template: &Raster, width: u32, height: u32, samples: Samples, out: &'a mut W) -> Result<TileStream<'a, W>> {
        template.require_coordinates().map_err(err)?;
        let gt = template.transform;
        if gt.pixel_height > 0.0 || gt.pixel_width < 0.0 {
            return Err(err("only north-up rasters can be exported for now"));
        }
        if width == 0 || height == 0 {
            return Err(err("the raster has no pixels to write"));
        }
        let n = template.bands.len().max(1);
        let bytes = samples == Samples::Hillshade;
        if bytes && template.nodata != Some(0.0) {
            return Err(err("a hillshade's pixels with no value are 0"));
        }
        if bytes && n > 1 {
            return Err(err("a hillshade has one band"));
        }
        // A class overview picks a pixel band by band, which for several bands could pick one
        // pixel's colour and another's alpha.
        if samples == Samples::Classes && n > 1 {
            return Err(err("classes are written one band at a time"));
        }
        if n > Self::most_bands() {
            return Err(err(format!(
                "its {n} bands make tiles of {} MB, more than a reader takes: at most {} bands are written a row of tiles at a time",
                (n as u64 * u64::from(TILE) * u64::from(TILE) * 4) >> 20,
                Self::most_bands()
            )));
        }
        let plan = stream_plan(bytes, template.nodata, n, &template.color);
        // With alpha, alpha says which pixels have a value, as [`write_to`] writes it: no no-data
        // value is written, a pixel with none is see-through, its colours what they are (a black
        // one stays black), and an integer template's alpha is held on a float's scale, opaque
        // at 1. Further bands mark a value with none NaN.
        let opaque = super::alpha_max(&template.data_type);
        let alpha_scale = plan.alpha.filter(|_| !bytes).map(|a| (a, opaque));
        let file_nodata = if alpha_scale.is_some() { None } else { template.nodata };
        let geo = geo_keys(template)?;
        let mut levels = vec![Level::new(width, height, 1)];
        while let Some(last) = levels.last().filter(|l| l.w.max(l.h) > TILE) {
            if levels.len() >= MAX_LEVELS {
                return Err(err("too many overviews"));
            }
            let (w, h, f) = (last.w.div_ceil(2), last.h.div_ceil(2), last.factor * 2);
            levels.push(Level::new(w, h, f));
        }
        let tiles: usize = levels.iter().map(Level::tiles).sum();
        // The worst a file can come to, as [`write_to`] reckons it.
        let raw = (tiles as u64).saturating_mul(u64::from(TILE) * u64::from(TILE) * if bytes { 1 } else { 4 }).saturating_mul(n as u64);
        let worst = raw.saturating_add(raw / 512).saturating_add(tiles as u64 * 64).saturating_add(1 << 20);
        let big = worst >= u64::from(u32::MAX);
        // A header past what a reader takes at a file's start couldn't be read in place: its
        // tiles' tables alone, an offset and a count each, refused before they are made.
        let table_bytes = (tiles as u64).saturating_mul(if big { 16 } else { 8 });
        if table_bytes > super::cog::MAX_HEADER {
            return Err(err(format!(
                "its tables of {tiles} tiles ({} MB) are more than a reader takes at a file's start: clip it to a smaller area first",
                table_bytes >> 20
            )));
        }
        let given_nodata = template.nodata.map_or(f32::NAN, |d| d as f32);
        let nodata = file_nodata.map_or(f32::NAN, |d| d as f32);
        let ifds: Vec<Vec<Entry>> = levels
            .iter()
            .enumerate()
            .map(|(i, lv)| {
                let mut e = entries(template, &plan, lv, i == 0, &geo, big);
                if i == 0 {
                    // What marks a pixel with none: the template's value, else NaN (its own
                    // pixels, which `entries` looks at, aren't the stream's); with alpha, alpha.
                    e.retain(|e| e.tag != 42113);
                    if alpha_scale.is_none() {
                        e.push(ascii(42113, &file_nodata.map_or_else(|| "nan".to_string(), |d| d.to_string())));
                    }
                }
                e
            })
            .collect();
        let head = build_head(&ifds, big);
        // A header past what a reader takes at its start couldn't be read in place.
        if head.bytes.len() as u64 > super::cog::MAX_HEADER {
            return Err(err(format!(
                "its tables of {tiles} tiles ({} MB) are more than a reader takes at a file's start: clip it to a smaller area first",
                head.bytes.len() >> 20
            )));
        }
        out.seek(SeekFrom::Start(0)).map_err(err)?;
        out.write_all(&head.bytes).map_err(err)?;
        let count = levels.len();
        Ok(TileStream {
            out,
            pos: head.bytes.len() as u64,
            places: levels.iter().map(|l| Vec::with_capacity(l.tiles())).collect(),
            levels,
            plan,
            pending: vec![vec![Vec::new(); n]; count],
            given: vec![0; count],
            patches: head.patches,
            big,
            nodata,
            given_nodata,
            alpha_scale,
            bytes,
            classes: samples == Samples::Classes,
            kept: None,
            template: template.clone(),
        })
    }

    /// How many bands it writes.
    fn bands(&self) -> usize {
        self.plan.bands.len()
    }

    /// The most bands a stream writes: their samples interleaved in a tile within what a reader
    /// decodes of one (`cog::MAX_TILE_BYTES`).
    pub fn most_bands() -> usize {
        super::cog::MAX_TILE_BYTES / (TILE as usize * TILE as usize * 4)
    }

    /// The level [`TileStream::keeping`] keeps of `levels` with `samples`: the finest whose tiles
    /// hold at most that many samples of `bands` bands, padding counted, else the coarsest.
    fn kept_level(levels: &[Level], bands: usize, samples: usize) -> usize {
        let tile = TILE as usize * TILE as usize * bands.max(1);
        levels.iter().position(|l| l.tiles().saturating_mul(tile) <= samples).unwrap_or(levels.len().saturating_sub(1))
    }

    /// How a pixel's value of a band is held in the file ([`Holding`]).
    fn holding(&self) -> Holding {
        Holding { given: self.given_nodata, nodata: self.nodata, alpha: self.alpha_scale }
    }

    /// Keeps, as it is written, the finest level whose tiles hold at most `samples` samples, every
    /// band's and their padding counted (the coarsest when none does), which
    /// [`TileStream::finish`] gives as a raster: an overview of the file to hold, the one a layer
    /// reading the file in place picks with the same budget, without reading it back.
    pub fn keeping(mut self, samples: usize) -> TileStream<'a, W> {
        let level = Self::kept_level(&self.levels, self.bands(), samples);
        self.kept = Some((level, vec![Vec::new(); self.bands()]));
        self
    }

    /// The most bytes a stream of a `width` × `height` image of `bands` bands holds at once,
    /// keeping an overview of at most `kept` samples ([`TileStream::keeping`]), given its rows a
    /// row of tiles at a time: each level's rows waiting for their row of tiles (none at the full
    /// size, which is written from the rows given; less than a row of tiles of each level after
    /// it, and the rows made for it), the tiles' tables, the overview kept, and the tiles being
    /// compressed.
    pub fn peak(width: u32, height: u32, bands: usize, kept: usize) -> u64 {
        let bands = bands.max(1) as u64;
        let row = u64::from(width).saturating_mul(u64::from(TILE)).saturating_mul(4).saturating_mul(bands);
        // The tiles of every level, each with an offset and a count of 8 bytes, held three times
        // as the header is laid out, and once more where each tile went.
        let (mut levels, (mut w, mut h)) = (Vec::new(), (width, height));
        loop {
            levels.push(Level::new(w, h, 1));
            if w.max(h) <= TILE || levels.len() >= MAX_LEVELS {
                break;
            }
            (w, h) = (w.div_ceil(2), h.div_ceil(2));
        }
        let tiles = levels.iter().map(|l| l.tiles() as u64).fold(0u64, u64::saturating_add);
        // The level kept, as many samples as it has (the coarsest can pass `kept`).
        let kept = levels
            .get(Self::kept_level(&levels, bands as usize, kept))
            .map_or(0, |l| (l.w as u64).saturating_mul(u64::from(l.h)).saturating_mul(bands));
        // Rows waiting at each level after the first: at most a row of tiles of that level and the
        // half-size rows just made for it, each level half as wide as the one before, so two
        // full-width rows hold them all.
        let rows = row.saturating_mul(2);
        let tables = tiles.saturating_mul(64);
        let tile = (u64::from(TILE) * u64::from(TILE) * 4).saturating_mul(bands);
        // No more tiles are made at once than a row of them has.
        let workers = (crate::threads::max() as u64).min(u64::from(width.div_ceil(TILE)).max(1));
        let compressing = workers.saturating_mul(tile.saturating_mul(2 + AHEAD as u64));
        rows.saturating_add(tables).saturating_add(kept.saturating_mul(4)).saturating_add(compressing)
    }

    /// The full image's next rows of its one band, whole rows of its width, top to bottom: each
    /// row of tiles they complete is written, and its overviews' rows made. More rows than the
    /// image has are an error, as is a stream of several bands.
    pub fn put(&mut self, rows: &[f32]) -> Result<()> {
        if self.bands() != 1 {
            return Err(err(format!("a stream of {} bands is given them all at once (put_bands)", self.bands())));
        }
        self.put_level(0, &[rows])
    }

    /// [`TileStream::put`] of every band: the same rows of each, in the bands' order.
    pub fn put_bands(&mut self, bands: &[&[f32]]) -> Result<()> {
        self.put_level(0, bands)
    }

    /// [`TileStream::put_bands`] at level `i`.
    fn put_level(&mut self, i: usize, rows: &[&[f32]]) -> Result<()> {
        let Some(lv) = self.levels.get(i) else { return Ok(()) };
        let w = lv.w as usize;
        let len = rows.first().map_or(0, |r| r.len());
        if rows.len() != self.bands() || rows.iter().any(|r| r.len() != len) {
            return Err(err(format!("the same rows of each of its {} bands must be given", self.bands())));
        }
        if !len.is_multiple_of(w) {
            return Err(err("rows must be whole rows of the image's width"));
        }
        let count = (len / w) as u64;
        let given = self.given.get(i).copied().unwrap_or(0).saturating_add(count);
        if given > u64::from(lv.h) {
            return Err(err(format!("more than the image's {} rows were given", lv.h)));
        }
        if let Some(g) = self.given.get_mut(i) {
            *g = given;
        }
        if let Some((_, kept)) = self.kept.as_mut().filter(|(level, _)| *level == i) {
            // Set aside whole at its first rows, as planned ([`TileStream::peak`]), not grown.
            let whole = w.saturating_mul(lv.h as usize);
            for (k, r) in kept.iter_mut().zip(rows) {
                k.try_reserve_exact(whole.max(k.len() + r.len()) - k.len()).map_err(|_| err("too little memory for the overview kept"))?;
                k.extend_from_slice(r);
            }
        }
        let tile_rows = TILE as usize * w;
        // Taken while its rows are written, which give the next level its own.
        let mut pending = self.pending.get_mut(i).map(std::mem::take).unwrap_or_default();
        pending.resize_with(rows.len(), Vec::new);
        let held = |pending: &[Vec<f32>]| pending.first().map_or(0, Vec::len);
        let add = |pending: &mut [Vec<f32>], from: usize, to: usize| -> Result<()> {
            for (p, r) in pending.iter_mut().zip(rows) {
                let part = r.get(from..to).unwrap_or_default();
                p.try_reserve(part.len()).map_err(|_| err("too little memory for the rows of a level waiting to be written"))?;
                p.extend_from_slice(part);
            }
            Ok(())
        };
        let mut at = 0;
        // A row of tiles begun: finished from the rows given, and written.
        if held(&pending) > 0 {
            let to = tile_rows.saturating_sub(held(&pending)).min(len);
            add(&mut pending, 0, to)?;
            at = to;
            if held(&pending) == tile_rows {
                let row: Vec<&[f32]> = pending.iter().map(Vec::as_slice).collect();
                self.write_row(i, &row)?;
                pending.iter_mut().for_each(Vec::clear);
            }
        }
        // Whole rows of tiles are written from the rows given, not copied first.
        while held(&pending) == 0 && len - at >= tile_rows {
            let row: Vec<&[f32]> = rows.iter().map(|r| r.get(at..at + tile_rows).unwrap_or_default()).collect();
            self.write_row(i, &row)?;
            at += tile_rows;
        }
        add(&mut pending, at, len)?;
        if let Some(p) = self.pending.get_mut(i) {
            *p = pending;
        }
        Ok(())
    }

    /// Writes `rows` (at most [`TILE`] of level `i`'s rows of each band, the last row of tiles
    /// when fewer) as the level's next row of tiles, then gives the next level their average two
    /// by two.
    fn write_row(&mut self, i: usize, rows: &[&[f32]]) -> Result<()> {
        let Some(lv) = self.levels.get(i) else { return Ok(()) };
        let (w, across) = (lv.w as usize, lv.across as usize);
        let (t, given, bytes, spp, holding) = (TILE as usize, self.given_nodata, self.bytes, self.bands(), self.holding());
        // The full image's values, or a classes' overview's, which picks a pixel: a value with
        // none there is one.
        let full = i == 0 || self.classes;
        let size = if bytes { 1 } else { 4 };
        let line = t * spp * size;
        let (sample, predictor) = (self.plan.sample, predictor(&self.plan));
        let make = |scratch: &mut Vec<u8>, tx: usize| -> Result<Vec<u8>> {
            let mut raw = Vec::with_capacity(t * t * spp * size);
            for y in 0..t {
                for x in 0..t {
                    let col = tx * t + x;
                    for (b, band) in rows.iter().enumerate() {
                        let v = if col < w { band.get(y * w + col).copied().unwrap_or(given) } else { given };
                        let v = holding.of(b, v, full);
                        if bytes {
                            raw.push(v.round().clamp(0.0, 255.0) as u8);
                        } else {
                            raw.extend_from_slice(&v.to_le_bytes());
                        }
                    }
                }
            }
            predict_rows(&mut raw, line, spp, sample, predictor, scratch);
            Ok(miniz_oxide::deflate::compress_to_vec_zlib(&raw, DEFLATE_FAST))
        };
        let (big, out, pos, places) = (self.big, &mut *self.out, &mut self.pos, &mut self.places);
        // The run's progress counts the caller's pieces, not these tiles.
        crate::progress::without_counting(|| {
            in_order(across, t * t * spp * size, err, Vec::new, make, |_, zipped| {
                if !big && *pos + zipped.len() as u64 > u64::from(u32::MAX) {
                    return Err(err("the file is too large for classic TIFF"));
                }
                out.write_all(&zipped).map_err(err)?;
                if let Some(p) = places.get_mut(i) {
                    p.push((*pos, zipped.len() as u64));
                }
                *pos += zipped.len() as u64;
                Ok(())
            })
        })?;
        // The next level's rows: each the average of two of these (the last alone when they are
        // odd), two pixels across at a time, each band by itself.
        if i + 1 < self.levels.len() {
            // Colours before an alpha band weighted by it; the rest by themselves.
            let alpha = self.alpha_scale.and_then(|(a, _)| rows.get(a).copied().map(|r| (a, r)));
            let half: Vec<Vec<f32>> = rows
                .iter()
                .enumerate()
                .map(|(b, r)| match alpha {
                    _ if self.classes => pick_rows(r, w, given),
                    Some((a, ar)) if b < a => average_by_alpha(r, ar, w, given, if i == 0 { given } else { f32::NAN }),
                    // A see-through pixel's alpha is 0, which an overview's averages in.
                    Some((a, _)) if b == a => {
                        // Only the full image's alpha can be given without a value: an overview's
                        // is a mean of alphas, whatever it comes to.
                        let seen: Vec<f32> = r.iter().map(|v| if v.is_nan() || (i == 0 && *v == given) { 0.0 } else { *v }).collect();
                        average_rows(&seen, w, f32::NAN)
                    }
                    _ => average_rows(r, w, given),
                })
                .collect();
            let half: Vec<&[f32]> = half.iter().map(Vec::as_slice).collect();
            self.put_level(i + 1, &half)?;
        }
        Ok(())
    }

    /// Writes what is left of each level once every row is given, and fills in the tile tables:
    /// what was written, and the level kept ([`TileStream::keeping`]), placed as a reader of the
    /// file places it, with `overview` the full image's size when it is coarser.
    pub fn finish(mut self) -> Result<(CogInfo, Option<Raster>)> {
        if self.given.first().copied() != self.levels.first().map(|l| u64::from(l.h)) {
            return Err(err("the image's rows weren't all given"));
        }
        for i in 0..self.levels.len() {
            let rest = self.pending.get_mut(i).map(std::mem::take).unwrap_or_default();
            if rest.first().is_some_and(|r| !r.is_empty()) {
                let row: Vec<&[f32]> = rest.iter().map(Vec::as_slice).collect();
                self.write_row(i, &row)?;
            }
        }
        for (lv, p) in self.levels.iter().zip(&self.places) {
            if p.len() != lv.tiles() {
                return Err(err(format!("{} of the {} tiles of a level were written", p.len(), lv.tiles())));
            }
        }
        fill_tables(self.out, &self.patches, &self.places, self.big, self.pos, err)?;
        let info = CogInfo { big: self.big, levels: self.levels.iter().map(|l| (l.w, l.h)).collect(), bytes: self.pos };
        let holding = self.holding();
        let (Some((level, data)), Some(full)) = (self.kept, self.levels.first()) else { return Ok((info, None)) };
        let Some(lv) = self.levels.get(level) else { return Ok((info, None)) };
        let t = &self.template;
        // A reader of the file spreads a coarser level over the full image's ground.
        let (sx, sy) = (f64::from(full.w) / f64::from(lv.w.max(1)), f64::from(full.h) / f64::from(lv.h.max(1)));
        let gt =
            super::GeoTransform { pixel_width: t.transform.pixel_width * sx, pixel_height: t.transform.pixel_height * sy, ..t.transform };
        // As the file holds it: bytes rounded as they were written, an integer template's alpha
        // on a float's scale, no-data as the file says it.
        let mut data = data;
        for (b, band) in data.iter_mut().enumerate() {
            band.iter_mut().for_each(|v| *v = holding.of(b, *v, level == 0 || self.classes));
        }
        let (data, kind): (Vec<Vec<f32>>, _) = match self.bytes {
            true => (data.into_iter().map(|b| b.into_iter().map(|v| v.round().clamp(0.0, 255.0)).collect()).collect(), "UInt8"),
            false => (data, "Float32"),
        };
        let nodata = (self.alpha_scale.is_none() && !self.nodata.is_nan()).then_some(f64::from(self.nodata)).and(t.nodata);
        // Its colour as the file says it: red, green and blue, grey and alpha, else grey.
        let color = match (&t.color, self.plan.rgb) {
            (ColorHint::Rgb { .. }, true) => ColorHint::Rgb { alpha: self.plan.alpha.is_some() },
            (ColorHint::GrayAlpha, false) if self.plan.alpha.is_some() => ColorHint::GrayAlpha,
            _ => ColorHint::Gray,
        };
        let mut r = Raster::new(t.name.clone(), lv.w, lv.h, data, gt, t.crs, nodata, color, kind)?;
        if level > 0 {
            r.overview = Some((full.w, full.h));
        }
        for (b, tb) in r.bands.iter_mut().zip(&t.bands) {
            b.name.clone_from(&tb.name);
            b.wavelength = tb.wavelength;
        }
        Ok((info, Some(r)))
    }
}

/// What a [`TileStream`] writes: `n` bands of 32-bit floats, or with `bytes` one band of bytes
/// whose no-data value is `nodata`, averaged in its overviews; red, green and blue first for an
/// RGB `color` of three bands or more (the fourth alpha when it says so), grey and alpha first for
/// grey and alpha, else grey, the further bands nothing in particular.
fn stream_plan(bytes: bool, nodata: Option<f64>, n: usize, color: &ColorHint) -> Plan {
    let (sample, nodata_int) = match bytes {
        true => (Sample::Byte, nodata.filter(|d| (0.0..=255.0).contains(d)).map(|d| d as u16)),
        false => (Sample::Float, None),
    };
    let (rgb, alpha) = match color {
        ColorHint::Rgb { alpha } if n >= 3 => (true, (*alpha && n >= 4).then_some(3)),
        ColorHint::GrayAlpha if n >= 2 => (false, Some(1)),
        _ => (false, None),
    };
    Plan { bands: (0..n).collect(), sample, rgb, alpha, nodata_int, palette: None, categorical: false }
}

/// How a [`TileStream`] holds a pixel's value in its file: `given` (the template's no-data, or
/// NaN) and NaN as `nodata`, the file's. With `alpha` (its band, and the value it is opaque at),
/// alpha says which pixels have a value instead: its own values divided by that value, 0 (see
/// through) for a pixel with none; the colour bands before it as they are, a black one black;
/// further bands NaN for a value with none.
#[derive(Clone, Copy)]
struct Holding {
    given: f32,
    nodata: f32,
    alpha: Option<(usize, f32)>,
}

impl Holding {
    /// Band `b`'s value `v` as the file holds it, of the full image when `full`: an overview's
    /// alpha is a mean of alphas, a pixel with none already 0, whatever value it comes to.
    fn of(&self, b: usize, v: f32, full: bool) -> f32 {
        let none = v.is_nan() || v == self.given;
        match self.alpha {
            None if none => self.nodata,
            None => v,
            Some((a, _)) if b == a && (v.is_nan() || (full && none)) => 0.0,
            Some((a, opaque)) if b == a => v / opaque,
            Some((a, _)) if b < a => v,
            Some(_) if none => f32::NAN,
            Some(_) => v,
        }
    }
}

/// `rows` of `w` pixels of a colour band made half the size each way as [`average_rows`] makes
/// them, each pixel weighted by `alpha`'s (the same rows of the alpha band, `alpha_none` marking
/// one with none, which counts for nothing), so see-through pixels don't darken an overview's
/// colours; the plain mean where all four are see-through.
fn average_by_alpha(rows: &[f32], alpha: &[f32], w: usize, nodata: f32, alpha_none: f32) -> Vec<f32> {
    let h = rows.len() / w.max(1);
    let (hw, hh) = (w.div_ceil(2), h.div_ceil(2));
    let valid = |v: f32| v.is_finite() && v != alpha_none;
    let mut out = Vec::with_capacity(hw * hh);
    for y in 0..hh {
        for x in 0..hw {
            let (mut sum, mut weight) = (0.0f64, 0.0f64);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (yy, xx) = (2 * y + dy, 2 * x + dx);
                if yy < h
                    && xx < w
                    && let (Some(&v), Some(&a)) = (rows.get(yy * w + xx), alpha.get(yy * w + xx))
                    // Alpha says whether a colour has a value: a black one is black.
                    && v.is_finite()
                    && valid(a)
                    && a > 0.0
                {
                    sum += f64::from(v) * f64::from(a);
                    weight += f64::from(a);
                }
            }
            if weight > 0.0 {
                out.push((sum / weight) as f32);
                continue;
            }
            // All four see-through: the plain mean of those with a value, as [`average_rows`].
            let (mut plain, mut n) = (0.0f64, 0u32);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (yy, xx) = (2 * y + dy, 2 * x + dx);
                if yy < h
                    && xx < w
                    && let Some(&v) = rows.get(yy * w + xx)
                    && v.is_finite()
                    && v != nodata
                {
                    plain += f64::from(v);
                    n += 1;
                }
            }
            out.push(if n > 0 { (plain / f64::from(n)) as f32 } else { nodata });
        }
    }
    out
}

/// `rows` of `w` pixels made half the size each way, for classes: each pixel the first of the up to
/// four it covers that has a finite value (not `nodata`), row by row, else `nodata`: always a
/// class one of them is, never one between them.
fn pick_rows(rows: &[f32], w: usize, nodata: f32) -> Vec<f32> {
    let h = rows.len() / w.max(1);
    let (hw, hh) = (w.div_ceil(2), h.div_ceil(2));
    let valid = |v: f32| v.is_finite() && v != nodata;
    let mut out = Vec::with_capacity(hw * hh);
    for y in 0..hh {
        for x in 0..hw {
            let first = [(0, 0), (0, 1), (1, 0), (1, 1)]
                .into_iter()
                .filter(|(dy, dx)| 2 * y + dy < h && 2 * x + dx < w)
                .filter_map(|(dy, dx)| rows.get((2 * y + dy) * w + 2 * x + dx).copied())
                .find(|v| valid(*v));
            out.push(first.unwrap_or(nodata));
        }
    }
    out
}

/// `rows` of `w` pixels made half the size each way: each pixel the mean of the up to four it
/// covers that have a finite value (not `nodata`), else `nodata`; an odd last row or column
/// covers itself alone.
fn average_rows(rows: &[f32], w: usize, nodata: f32) -> Vec<f32> {
    let h = rows.len() / w.max(1);
    let (hw, hh) = (w.div_ceil(2), h.div_ceil(2));
    let valid = |v: f32| v.is_finite() && v != nodata;
    let mut out = Vec::with_capacity(hw * hh);
    for y in 0..hh {
        for x in 0..hw {
            let (mut sum, mut n) = (0.0f64, 0u32);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (yy, xx) = (2 * y + dy, 2 * x + dx);
                if yy < h
                    && xx < w
                    && let Some(&v) = rows.get(yy * w + xx)
                    && valid(v)
                {
                    sum += f64::from(v);
                    n += 1;
                }
            }
            out.push(if n > 0 { (sum / f64::from(n)) as f32 } else { nodata });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tiff::decoder::{Decoder, DecodingResult};
    use tiff::tags::Tag;

    use super::*;
    use crate::raster::{Crs, GeoTransform, geotiff};

    fn utm(w: u32, h: u32, bands: Vec<Vec<f32>>, color: ColorHint, data_type: &str, nodata: Option<f64>) -> Raster {
        let gt = GeoTransform { origin_x: 500_000.0, origin_y: 4_000_000.0, pixel_width: 30.0, pixel_height: -30.0 };
        Raster::new("t", w, h, bands, gt, Crs::from_epsg(32617).unwrap(), nodata, color, data_type).unwrap()
    }

    /// An overview's pixel is the mean of the pixels with a value it covers: none, NaN or infinite
    /// left out, an odd last row or column covering itself alone, and no-data where it covers
    /// none with one.
    #[test]
    fn overview_rows_average_the_pixels_with_a_value() {
        let nd = -9999.0;
        assert_eq!(average_rows(&[1.0, f32::NAN, 5.0], 3, nd), [1.0, 5.0]);
        assert_eq!(average_rows(&[f32::INFINITY, 2.0, 4.0, 6.0], 2, nd), [4.0]);
        assert_eq!(average_rows(&[nd, f32::NAN, 3.0, 8.0, nd, nd], 3, nd), [8.0, 3.0]);
        assert_eq!(average_rows(&[nd, nd, 1.0, 3.0], 2, nd), [2.0]);
        assert_eq!(average_rows(&[nd, f32::NEG_INFINITY], 2, nd), [nd]);
    }

    /// A classes' overview pixel is a class one of its four pixels is, the first with a value,
    /// never an average between two.
    #[test]
    fn overview_rows_of_classes_take_a_class() {
        let nd = -9999.0;
        assert_eq!(pick_rows(&[1.0, 3.0, 3.0, 1.0], 2, nd), [1.0]);
        assert_eq!(pick_rows(&[nd, f32::NAN, 2.0, 1.0], 2, nd), [2.0]);
        assert_eq!(pick_rows(&[nd, nd, 4.0, nd, nd, nd], 3, nd), [nd, 4.0]);
    }

    /// A hillshade's template (bytes, no-data 0) is streamed as bytes: it reads
    /// back as `UInt8`, whole numbers, its no-data kept, and the overview kept is bytes too.
    #[test]
    fn a_stream_of_bytes_is_written_as_bytes() {
        let (w, h) = (700u32, 600u32);
        let value = |x: u32, y: u32| if (x + y).is_multiple_of(53) { 0.0 } else { f32::from(((x * 3 + y) % 254 + 1) as u8) };
        let data: Vec<f32> = (0..h).flat_map(|y| (0..w).map(move |x| value(x, y))).collect();
        let template = utm(1, 1, vec![vec![0.0]], ColorHint::Gray, "UInt8", Some(0.0));
        let mut out = Cursor::new(Vec::new());
        let mut stream = TileStream::new(&template, w, h, Samples::Hillshade, &mut out).unwrap().keeping(1 << 18);
        stream.put(&data).unwrap();
        let (info, kept) = stream.finish().unwrap();
        assert_eq!(info.levels, vec![(700, 600), (350, 300)]);
        let kept = kept.unwrap();
        assert_eq!((kept.width, kept.data_type.as_str(), kept.nodata), (350, "UInt8", Some(0.0)));
        assert!(kept.bands[0].data.iter().all(|v| v.fract() == 0.0 && (0.0..=255.0).contains(v)));
        let back = geotiff::read(Cursor::new(out.into_inner()), "back").unwrap();
        assert_eq!((back.width, back.height, back.data_type.as_str(), back.nodata), (w, h, "UInt8", Some(0.0)));
        for y in 0..h {
            for x in 0..w {
                let v = value(x, y);
                assert_eq!(back.value(0, i64::from(x), i64::from(y)), (v != 0.0).then_some(v), "{x},{y}");
            }
        }
    }

    /// A raster streamed in pieces of uneven rows (#2865) reads back pixel for pixel, its no-data
    /// value kept, and each overview the average two by two of the level before; the layout's
    /// reader finds every level.
    #[test]
    fn a_streamed_raster_reads_back_with_its_overviews() {
        let (w, h) = (1300u32, 1100u32);
        let value = |x: u32, y: u32| if (x * 7 + y * 3).is_multiple_of(97) { f32::NAN } else { (x as f32) * 0.5 - (y as f32) * 0.25 };
        let data: Vec<f32> = (0..h).flat_map(|y| (0..w).map(move |x| value(x, y))).collect();
        let template = utm(1, 1, vec![vec![0.0]], ColorHint::Gray, "Float32", Some(-9999.0));
        let mut out = Cursor::new(Vec::new());
        let mut stream = TileStream::new(&template, w, h, Samples::Floats, &mut out).unwrap();
        let mut at = 0usize;
        for rows in [1usize, 300, 511, 7, 281] {
            let n = rows * w as usize;
            stream.put(&data[at..at + n]).unwrap();
            at += n;
        }
        assert!(stream.put(&data[..w as usize]).is_err(), "a row past the image");
        let (info, kept) = stream.finish().unwrap();
        assert_eq!(info.levels, vec![(1300, 1100), (650, 550), (325, 275)]);
        assert!(kept.is_none(), "nothing kept unless asked");
        let bytes = out.into_inner();
        let back = geotiff::read(Cursor::new(&bytes), "back").unwrap();
        assert_eq!((back.width, back.height, back.nodata, back.transform), (w, h, Some(-9999.0), template.transform));
        for y in 0..h {
            for x in 0..w {
                let want = value(x, y);
                let got = back.value(0, i64::from(x), i64::from(y));
                assert_eq!(got, (!want.is_nan()).then_some(want), "{x},{y}");
            }
        }
        // Each overview is its level before averaged two by two.
        let mut before = data.iter().map(|v| if v.is_nan() { -9999.0 } else { *v }).collect::<Vec<f32>>();
        let mut bw = w as usize;
        for (lw, lh) in [(650u32, 550u32), (325, 275)] {
            let want = average_rows(&before, bw, -9999.0);
            let level = geotiff::read_limited(geotiff::reader(Cursor::new(&bytes)).unwrap(), "o", (lw * lh) as usize, None).unwrap();
            assert_eq!((level.width, level.height), (lw, lh));
            assert_eq!(level.bands[0].data, want, "the {lw} × {lh} overview");
            (before, bw) = (want, lw as usize);
        }
        let Ok(crate::raster::cog::Parse::Done(layout)) = crate::raster::cog::layout(&crate::raster::cog::Sparse::default().with(0, bytes))
        else {
            panic!("not a tiled GeoTIFF the reader reads in place");
        };
        assert_eq!(layout.levels.len(), 3);
        // Rows that aren't whole rows, and an image left short, are errors.
        let mut out = Cursor::new(Vec::new());
        let mut stream = TileStream::new(&template, w, h, Samples::Floats, &mut out).unwrap();
        assert!(stream.put(&data[..5]).is_err());
        stream.put(&data[..w as usize]).unwrap();
        assert!(stream.finish().is_err());
    }

    /// A stream of several bands (#2865): an RGB template's three are written as red, green and
    /// blue, five plain ones as grey with four more, each read back pixel for pixel, with its
    /// name, the colour it was given, and its overviews averaged band by band; the overview kept
    /// holds every band. Bands given unevenly are an error.
    #[test]
    fn a_streamed_raster_of_several_bands_reads_back_band_by_band() {
        let (w, h) = (700u32, 600u32);
        let value = |b: usize, x: u32, y: u32| {
            if (x * 7 + y * 3 + b as u32).is_multiple_of(89) { f32::NAN } else { (x as f32) * 0.5 - (y as f32) * 0.25 + b as f32 * 100.0 }
        };
        for (n, color) in [
            (3usize, ColorHint::Rgb { alpha: false }),
            (5, ColorHint::Gray),
            (5, ColorHint::Rgb { alpha: false }),
            (3, ColorHint::GrayAlpha),
        ] {
            let bands: Vec<Vec<f32>> = (0..n).map(|b| (0..h).flat_map(|y| (0..w).map(move |x| value(b, x, y))).collect()).collect();
            let mut template = utm(1, 1, vec![vec![0.0]; n], color.clone(), "Float32", Some(-9999.0));
            template.bands.iter_mut().enumerate().for_each(|(b, band)| {
                band.name = format!("band {b}");
                band.wavelength = Some(500.0 + b as f64);
            });
            let mut out = Cursor::new(Vec::new());
            let mut stream = TileStream::new(&template, w, h, Samples::Floats, &mut out).unwrap().keeping(1 << 20);
            assert!(stream.put(&bands[0][..w as usize]).is_err(), "one band of {n}");
            let mut at = 0usize;
            for rows in [3usize, 509, 88] {
                let m = rows * w as usize;
                let rows: Vec<&[f32]> = bands.iter().map(|b| &b[at..at + m]).collect();
                stream.put_bands(&rows).unwrap();
                at += m;
            }
            let (info, kept) = stream.finish().unwrap();
            assert_eq!(info.levels, vec![(700, 600), (350, 300)], "{n} bands");
            let bytes = out.into_inner();
            let back = geotiff::read(Cursor::new(&bytes), "back").unwrap();
            // With alpha (grey and alpha), alpha says which pixels have a value, so no no-data
            // value is written and the pixels are as the alpha test below has them.
            let alpha = color == ColorHint::GrayAlpha;
            assert_eq!(
                (back.width, back.height, back.nodata, back.band_count(), &back.color),
                (w, h, (!alpha).then_some(-9999.0), n, &color),
                "{n} bands"
            );
            for (b, band) in back.bands.iter().enumerate() {
                assert_eq!(band.name, format!("band {b}"));
                for y in (0..h).filter(|_| !alpha) {
                    for x in 0..w {
                        let want = value(b, x, y);
                        assert_eq!(back.value(b, i64::from(x), i64::from(y)), (!want.is_nan()).then_some(want), "band {b} {x},{y}");
                    }
                }
            }
            let level = geotiff::read_limited(geotiff::reader(Cursor::new(&bytes)).unwrap(), "o", 350 * 300 * n, None).unwrap();
            let kept = kept.unwrap();
            assert_eq!((level.width, level.height, kept.width, kept.height, kept.band_count()), (350, 300, 350, 300, n));
            for (b, data) in bands.iter().enumerate().filter(|_| !alpha) {
                let filled: Vec<f32> = data.iter().map(|v| if v.is_nan() { -9999.0 } else { *v }).collect();
                let want = average_rows(&filled, w as usize, -9999.0);
                assert_eq!(level.bands[b].data, want, "{n} bands: band {b}'s overview");
                assert_eq!(kept.bands[b].data, want, "{n} bands: band {b} kept");
            }
            assert_eq!(kept.color, color);
            assert!(
                kept.bands.iter().enumerate().all(|(b, band)| band.wavelength == Some(500.0 + b as f64)),
                "{n} bands: wavelengths kept"
            );
            // The in-place reader finds every band and the colour too.
            let Ok(crate::raster::cog::Parse::Done(layout)) =
                crate::raster::cog::layout(&crate::raster::cog::Sparse::default().with(0, bytes))
            else {
                panic!("not a tiled GeoTIFF the reader reads in place");
            };
            assert_eq!((layout.bands, &layout.color, layout.levels.len()), (n, &color, 2), "{n} bands in place");
        }
        // With alpha, alpha says which pixels have a value: no no-data value is written, an
        // integer template's alpha is held on a float's scale, opaque at 1, a black pixel's colour
        // (the template's no-data, 0) stays black, a pixel with none is see-through, and an
        // overview's colours are weighted by alpha, a see-through pixel's counting for nothing.
        let (w, h) = (600u32, 600u32);
        let mut rgba = utm(1, 1, vec![vec![0.0]; 4], ColorHint::Rgb { alpha: true }, "UInt8", Some(0.0));
        rgba.bands.iter_mut().for_each(|b| b.name = String::new());
        let mut out = Cursor::new(Vec::new());
        let mut stream = TileStream::new(&rgba, w, h, Samples::Floats, &mut out).unwrap().keeping(1 << 20);
        let px = (w * h) as usize;
        let (mut red, mut colour, mut alpha) = (vec![50.0f32; px], vec![50.0f32; px], vec![255.0f32; px]);
        alpha[0] = 128.0;
        red[1] = 0.0;
        let at = |x: usize, y: usize| y * w as usize + x;
        for x in [2, 3] {
            (red[at(x, 2)], colour[at(x, 2)]) = (200.0, 200.0);
            (red[at(x, 3)], colour[at(x, 3)], alpha[at(x, 3)]) = (0.0, 0.0, 0.0);
        }
        stream.put_bands(&[&red, &colour, &colour, &alpha]).unwrap();
        let (_, kept) = stream.finish().unwrap();
        let back = geotiff::read(Cursor::new(out.into_inner()), "back").unwrap();
        assert_eq!((&back.color, back.nodata), (&ColorHint::Rgb { alpha: true }, None));
        assert_eq!((back.value(3, 1, 0), back.value(3, 0, 0), back.value(0, 1, 0)), (Some(1.0), Some(128.0 / 255.0), Some(0.0)));
        let kept = kept.unwrap();
        assert_eq!((kept.width, kept.nodata, &kept.color), (300, None, &ColorHint::Rgb { alpha: true }));
        let one = |b: usize, x: usize, y: usize| kept.bands[b].data[y * 300 + x];
        let corner = ((128.0f64 + 3.0 * 255.0) / 4.0) as f32 / 255.0;
        let red_corner = ((50.0 * 128.0 + 50.0 * 255.0 * 2.0) / (128.0 + 255.0 * 3.0)) as f32;
        assert_eq!((one(3, 0, 0), one(0, 0, 0), one(3, 2, 0)), (corner, red_corner, 1.0));
        assert_eq!((one(0, 1, 1), one(3, 1, 1)), (200.0, 0.5), "see-through pixels darken nothing and halve alpha");
        // An overview's alpha is a mean of alphas, kept whatever it comes to: one of 64 and 192 is
        // 128, the template's no-data value here, and still half see-through.
        let mut rgba = utm(1, 1, vec![vec![0.0]; 4], ColorHint::Rgb { alpha: true }, "UInt8", Some(128.0));
        rgba.bands.iter_mut().for_each(|b| b.name = String::new());
        let mut out = Cursor::new(Vec::new());
        let mut stream = TileStream::new(&rgba, w, h, Samples::Floats, &mut out).unwrap().keeping(1 << 20);
        let checker: Vec<f32> = (0..px).map(|i| if (i % w as usize + i / w as usize).is_multiple_of(2) { 64.0 } else { 192.0 }).collect();
        let colour = vec![50.0f32; px];
        stream.put_bands(&[&colour, &colour, &colour, &checker]).unwrap();
        let (_, kept) = stream.finish().unwrap();
        let bytes = out.into_inner();
        let level = geotiff::read_limited(geotiff::reader(Cursor::new(&bytes)).unwrap(), "o", 300 * 300 * 4, None).unwrap();
        assert_eq!(level.width, 300);
        assert!(level.bands[3].data.iter().all(|a| *a == 128.0 / 255.0), "the file's overview");
        assert!(kept.unwrap().bands[3].data.iter().all(|a| *a == 128.0 / 255.0), "the overview kept");
        // More bands than a reader decodes a tile of are refused; the plan counts the overview
        // kept, the coarsest level when none fits.
        let many = utm(1, 1, vec![vec![0.0]; TileStream::<Cursor<Vec<u8>>>::most_bands() + 1], ColorHint::Gray, "Float32", None);
        assert!(TileStream::new(&many, 10, 10, Samples::Floats, &mut Cursor::new(Vec::new())).is_err());
        let peak = |kept: usize| TileStream::<Cursor<Vec<u8>>>::peak(2048, 2048, 32, kept);
        assert_eq!(peak(0), peak(512 * 512 * 32), "the 512 × 512 level kept either way");
        // A hillshade is one band.
        let shade = utm(1, 1, vec![vec![0.0]; 2], ColorHint::Gray, "UInt8", Some(0.0));
        assert!(TileStream::new(&shade, 10, 10, Samples::Hillshade, &mut Cursor::new(Vec::new())).is_err());
    }

    /// 3000 × 2000 colour pixels: red rises across, green down, blue is a checker of 2 × 2 blocks.
    fn big_rgb() -> Raster {
        let (w, h) = (3000u32, 2000u32);
        let mut bands = vec![Vec::new(), Vec::new(), Vec::new()];
        for y in 0..h {
            for x in 0..w {
                bands[0].push((x % 256) as f32);
                bands[1].push((y % 256) as f32);
                bands[2].push(if (x / 2 + y / 2) % 2 == 0 { 200.0 } else { 100.0 });
            }
        }
        utm(w, h, bands, ColorHint::Rgb { alpha: false }, "UInt8", None)
    }

    /// What `dec` says about the image it is on: size, tile size, tile offsets, compression.
    fn level(dec: &mut Decoder<Cursor<&[u8]>>) -> ((u32, u32), u32, Vec<u64>, u16) {
        let tw = dec.find_tag_unsigned::<u32>(Tag::TileWidth).unwrap().unwrap();
        let offsets = dec.find_tag_unsigned_vec::<u64>(Tag::TileOffsets).unwrap().unwrap();
        let comp = dec.find_tag_unsigned::<u16>(Tag::Compression).unwrap().unwrap();
        (dec.dimensions().unwrap(), tw, offsets, comp)
    }

    #[test]
    fn layout_is_a_cog() {
        let r = big_rgb();
        let bytes = write(&r, 0).unwrap();
        assert_eq!(&bytes[..4], b"II*\0", "classic TIFF when it fits");
        let mut dec = Decoder::new(Cursor::new(bytes.as_slice())).unwrap();
        let mut levels = Vec::new();
        loop {
            levels.push(level(&mut dec));
            if !dec.more_images() {
                break;
            }
            dec.next_image().unwrap();
        }
        // Each overview half the last, down to one tile.
        let sizes: Vec<_> = levels.iter().map(|l| l.0).collect();
        assert_eq!(sizes, [(3000, 2000), (1500, 1000), (750, 500), (375, 250)]);
        assert!(levels.iter().all(|l| l.1 == 512 && l.3 == 8));
        // Tiles: 6 × 4, 3 × 2, 2 × 1, 1 × 1.
        assert_eq!(levels.iter().map(|l| l.2.len()).collect::<Vec<_>>(), [24, 6, 2, 1]);
        // The smallest overview's tiles come first and the full image's last, each in file order,
        // and every directory lies before the first tile.
        let mut last = 0;
        for l in levels.iter().rev() {
            for o in &l.2 {
                assert!(*o > last, "tiles run forward through the file");
                last = *o;
            }
        }
        let first_tile = levels.last().unwrap().2[0];
        assert!(first_tile < levels[0].2[0]);
        let mut ifds_end = 0usize;
        // The directories chain from byte 8; the last chain pointer is 0.
        let mut at = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        for _ in 0..levels.len() {
            let n = u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()) as usize;
            ifds_end = at + 2 + 12 * n + 4;
            at = u32::from_le_bytes(bytes[ifds_end - 4..ifds_end].try_into().unwrap()) as usize;
        }
        assert_eq!(at, 0);
        assert!((ifds_end as u64) < first_tile);
    }

    #[test]
    fn reads_back_with_the_same_georeferencing_and_pixels() {
        let r = big_rgb();
        let back = geotiff::read(Cursor::new(write(&r, 0).unwrap()), "back").unwrap();
        assert_eq!((back.width, back.height, back.band_count()), (3000, 2000, 3));
        assert_eq!(back.crs.epsg(), 32617);
        assert_eq!(back.transform, r.transform);
        assert_eq!(back.color, ColorHint::Rgb { alpha: false });
        assert_eq!(back.data_type, "UInt8");
        for (b, x, y) in [(0, 0, 0), (0, 2999, 1999), (1, 1500, 1234), (2, 511, 511), (2, 512, 513), (0, 1024, 7)] {
            assert_eq!(back.value(b, x, y), r.value(b, x, y), "band {b} at {x},{y}");
        }
        // The same pixels, all of them.
        assert!(back.bands.iter().zip(&r.bands).all(|(a, b)| a.data == b.data));
    }

    #[test]
    fn overviews_are_block_averages() {
        let r = big_rgb();
        let bytes = write(&r, 0).unwrap();
        let mut dec = Decoder::new(Cursor::new(bytes.as_slice())).unwrap();
        // Overview 2 (index 2): 750 × 500, a block of 4 × 4 full pixels each.
        dec.seek_to_image(2).unwrap();
        assert_eq!(dec.dimensions().unwrap(), (750, 500));
        let DecodingResult::U8(px) = dec.read_image().unwrap() else { panic!("bytes") };
        let at = |x: usize, y: usize, b: usize| px[(y * 750 + x) * 3 + b];
        // Blue: 2 × 2 blocks alternate 200 and 100, so any 4 × 4 block averages 150.
        assert_eq!(at(10, 10, 2), 150);
        // Red is x mod 256 over x in 4k..4k+4: mean 4k + 1.5 → rounds to 4k + 2 (k = 5 → 21.5 → 22).
        assert_eq!(at(5, 3, 0), 22);
        // Green: y mod 256 over 4j..4j+4 (j = 3 → 13.5 → 14).
        assert_eq!(at(0, 3, 1), 14);
    }

    #[test]
    fn raster_export_review_named_classes_keep_codes_in_float_overviews() {
        let (w, h) = (700u32, 4u32);
        let data = (0..w * h).map(|i| if i % w < 2 { f32::NAN } else { (i % 2) as f32 }).collect();
        let mut r = utm(w, h, vec![data], ColorHint::Gray, "UInt8", None);
        r.bands[0].classes = vec!["Water".into(), "Land".into()];
        let bytes = write(&r, 0).unwrap();
        let ov = geotiff::read_limited(geotiff::reader(Cursor::new(bytes.as_slice())).unwrap(), "o", 1500, None).unwrap();
        assert_eq!((ov.width, ov.height), (350, 2));
        assert_eq!(ov.value(0, 0, 0), None);
        assert!(ov.bands[0].data.iter().all(|v| v.is_nan() || *v == 0.0 || *v == 1.0), "no averaged class codes");
        assert_eq!(ov.bands[0].classes, r.bands[0].classes);
        r.bands[0].classes.clear();
        let bytes = write(&r, 0).unwrap();
        let ov = geotiff::read_limited(geotiff::reader(Cursor::new(bytes.as_slice())).unwrap(), "o", 1500, None).unwrap();
        assert_eq!(ov.value(0, 2, 0), Some(0.5), "continuous float overviews still average");
    }

    #[test]
    fn edges_of_odd_sizes_average_what_is_there() {
        // 1025 × 3 floats: the second overview's last column averages one pixel column, and the
        // one row of the overview below is 1 pixel tall.
        let (w, h) = (1025u32, 3u32);
        let data: Vec<f32> = (0..w * h).map(|i| (i % w) as f32).collect();
        let r = utm(w, h, vec![data], ColorHint::Gray, "Float32", None);
        let bytes = write(&r, 0).unwrap();
        let mut dec = Decoder::new(Cursor::new(bytes.as_slice())).unwrap();
        dec.seek_to_image(1).unwrap();
        assert_eq!(dec.dimensions().unwrap(), (513, 2));
        let DecodingResult::F32(px) = dec.read_image().unwrap() else { panic!("floats") };
        assert_eq!(px[0], 0.5);
        assert_eq!(px[512], 1024.0, "the last column is a lone pixel");
    }

    #[test]
    fn no_data_is_kept_and_skipped_in_averages() {
        let (w, h) = (600u32, 600u32);
        let mut data = vec![10.0f32; (w * h) as usize];
        // The first 2 × 2 block is half no-data; the second wholly.
        data[0] = -9999.0;
        data[1] = -9999.0;
        for i in [2, 3, w as usize + 2, w as usize + 3] {
            data[i] = -9999.0;
        }
        let r = utm(w, h, vec![data], ColorHint::Gray, "Float32", Some(-9999.0));
        let bytes = write(&r, 0).unwrap();
        let back = geotiff::read(Cursor::new(bytes.as_slice()), "b").unwrap();
        assert_eq!(back.nodata, Some(-9999.0));
        assert_eq!(back.value(0, 0, 0), None);
        assert_eq!(back.value(0, 4, 0), Some(10.0));
        let mut dec = Decoder::new(Cursor::new(bytes.as_slice())).unwrap();
        dec.seek_to_image(1).unwrap();
        let DecodingResult::F32(px) = dec.read_image().unwrap() else { panic!("floats") };
        assert_eq!(px[0], 10.0, "half valid: the mean of the valid pixels");
        assert_eq!(px[1], -9999.0, "no valid pixel: no data");
    }

    #[test]
    fn alpha_weights_the_colours_of_overviews() {
        let (w, h) = (600u32, 600u32);
        let n = (w * h) as usize;
        // Opaque white on the left half, see-through black on the right.
        let left = |i: usize| (i % w as usize) < 300;
        let mut bands: Vec<Vec<f32>> = (0..3).map(|_| (0..n).map(|i| if left(i) { 255.0 } else { 0.0 }).collect()).collect();
        bands.push((0..n).map(|i| if left(i) { 255.0 } else { 0.0 }).collect());
        let r = utm(w, h, bands, ColorHint::Rgb { alpha: true }, "UInt8", None);
        let bytes = write(&r, 0).unwrap();
        let back = geotiff::read(Cursor::new(bytes.as_slice()), "b").unwrap();
        assert_eq!((back.band_count(), back.color.clone()), (4, ColorHint::Rgb { alpha: true }));
        let mut dec = Decoder::new(Cursor::new(bytes.as_slice())).unwrap();
        dec.seek_to_image(1).unwrap();
        let DecodingResult::U8(px) = dec.read_image().unwrap() else { panic!("bytes") };
        // The overview's column 150 straddles nothing (300 / 2): colour stays white up to the edge.
        let at = |x: usize| &px[x * 4..x * 4 + 4];
        assert_eq!(at(149), [255, 255, 255, 255]);
        assert_eq!(at(150)[3], 0);
    }

    #[test]
    fn bigtiff_reads_back_the_same() {
        let r = big_rgb();
        let mut out = Cursor::new(Vec::new());
        let info = write_to(&r, 0, Flavour::Big, &mut out).unwrap();
        let bytes = out.into_inner();
        assert!(info.big && info.bytes == bytes.len() as u64);
        assert_eq!(&bytes[..4], b"II+\0");
        let back = geotiff::read(Cursor::new(bytes.as_slice()), "b").unwrap();
        assert!(back.bands.iter().zip(&r.bands).all(|(a, b)| a.data == b.data));
        assert_eq!(back.transform, r.transform);
        let mut dec = Decoder::new(Cursor::new(bytes.as_slice())).unwrap();
        let mut n = 1;
        while dec.more_images() {
            dec.next_image().unwrap();
            n += 1;
        }
        assert_eq!(n, 4);
        // Auto chooses classic for a small file; the classic estimate refuses nothing it can't write.
        assert!(!write_to(&r, 0, Flavour::Auto, &mut Cursor::new(Vec::new())).unwrap().big);
    }

    #[test]
    fn small_and_odd_rasters() {
        // One pixel: one tile, no overviews.
        let r = utm(1, 1, vec![vec![7.0]], ColorHint::Gray, "Float32", None);
        let bytes = write(&r, 0).unwrap();
        let back = geotiff::read(Cursor::new(bytes.as_slice()), "b").unwrap();
        assert_eq!(back.value(0, 0, 0), Some(7.0));
        assert!(!Decoder::new(Cursor::new(bytes.as_slice())).unwrap().more_images());
        // A grey byte raster stays bytes; the band argument picks the band.
        let r = utm(4, 2, vec![vec![1.0; 8], vec![2.0; 8]], ColorHint::Gray, "UInt8", None);
        let back = geotiff::read(Cursor::new(write(&r, 1).unwrap()), "b").unwrap();
        assert_eq!((back.data_type.as_str(), back.value(0, 3, 1)), ("UInt8", Some(2.0)));
        assert!(write(&r, 2).is_err());
        // Pixels with no value and no no-data value need floats.
        let r = utm(2, 1, vec![vec![5.0, f32::NAN]], ColorHint::Gray, "UInt8", None);
        let back = geotiff::read(Cursor::new(write(&r, 0).unwrap()), "b").unwrap();
        assert_eq!((back.data_type.as_str(), back.value(0, 0, 0), back.value(0, 1, 0)), ("Float32", Some(5.0), None));
    }

    #[test]
    fn refuses_what_it_cannot_place() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 0.0, pixel_width: 1.0, pixel_height: 1.0 };
        let r = Raster::new("up", 2, 2, vec![vec![1.0; 4]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        assert!(write(&r, 0).is_err(), "south-up grids are refused, as GeoTIFF export refuses them");
    }

    #[test]
    fn files_are_written_through_a_temporary() {
        let dir = std::env::temp_dir().join(format!("rustgis-cog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.tif");
        let r = big_rgb();
        let info = write_file(&r, 0, &path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), info.bytes);
        assert_eq!(info.levels.len(), 4);
        // Only the file is left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let back = crate::raster::read_path(&path).unwrap();
        assert_eq!((back.width, back.height), (3000, 2000));
        assert!(write_file(&r, 0, &dir.join("missing").join("x.tif")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classes_keep_their_codes_and_colour_table_in_overviews() {
        // Columns alternate 11 and 41: an average would invent 26.
        let (w, h) = (700u32, 700u32);
        let data: Vec<f32> = (0..w * h).map(|i| if i % 2 == 0 { 11.0 } else { 41.0 }).collect();
        let mut colors = vec![[0u8, 0, 0, 255]; 256];
        colors[11] = [200, 10, 10, 255];
        colors[41] = [10, 20, 230, 255];
        let r = utm(w, h, vec![data], ColorHint::Palette(colors.clone()), "UInt8", None);
        let bytes = write(&r, 0).unwrap();
        let back = geotiff::read(Cursor::new(bytes.as_slice()), "b").unwrap();
        assert!(matches!(&back.color, ColorHint::Palette(p) if p.get(11).map(|c| [c[0], c[1], c[2]]) == Some([200, 10, 10])));
        assert_eq!(back.value(0, 1, 0), Some(41.0));
        // The overview that fits 200,000 samples is read: 350 × 350, with its table.
        let ov = geotiff::read_limited(geotiff::reader(Cursor::new(bytes.as_slice())).unwrap(), "o", 200_000, None).unwrap();
        assert_eq!(ov.overview, Some((700, 700)));
        assert!(ov.bands[0].data.iter().all(|v| *v == 11.0 || *v == 41.0), "no invented classes");
        assert!(matches!(&ov.color, ColorHint::Palette(p) if p.get(41).map(|c| c[2]) == Some(230)));
        // A palette raster with holes: they get an unused index as no-data.
        let mut holes = vec![11.0f32; 4];
        holes[1] = f32::NAN;
        let h = utm(2, 2, vec![holes], ColorHint::Palette(vec![[0, 0, 0, 255]; 256]), "UInt8", None);
        let back = geotiff::read(Cursor::new(write(&h, 0).unwrap()), "h").unwrap();
        assert_eq!((back.nodata, back.value(0, 1, 0), back.value(0, 0, 0)), (Some(0.0), None, Some(11.0)));
        // Indexes that are not bytes can't keep a colour table: refused, not invented.
        let bad = utm(2, 1, vec![vec![1.5, 300.0]], ColorHint::Palette(colors), "Float32", None);
        assert!(write(&bad, 0).is_err());
    }

    #[test]
    fn each_kind_of_sample_reads_back_exactly_through_its_predictor() {
        // Wider than a tile, so rows run on into a second one and end in its padding.
        let (w, h) = (700u32, 3u32);
        let n = (w * h) as usize;
        let floats =
            (0..n).map(|i| if i % 97 == 0 { f32::NAN } else { (i as f32 - 1000.0) * 3.7e-4 * if i % 5 == 0 { 1e30 } else { 1.0 } });
        let cases = [
            (utm(w, h, vec![floats.collect()], ColorHint::Gray, "Float32", None), 3),
            (utm(w, h, vec![(0..n).map(|i| (i * 37 % 65_536) as f32).collect()], ColorHint::Gray, "UInt16", None), 2),
            (utm(w, h, vec![(0..n).map(|i| (i * 7 % 256) as f32).collect()], ColorHint::Gray, "UInt8", None), 2),
            // A few classes take no predictor.
            (utm(w, h, vec![(0..n).map(|i| (i % 3) as f32).collect()], ColorHint::Gray, "UInt8", None), 1),
        ];
        for (r, predictor) in cases {
            let bytes = write(&r, 0).unwrap();
            let mut dec = Decoder::new(Cursor::new(bytes.as_slice())).unwrap();
            assert_eq!(dec.find_tag_unsigned::<u16>(Tag::Predictor).unwrap(), Some(predictor), "{}", r.data_type);
            let back = geotiff::read(Cursor::new(bytes), "back").unwrap();
            for (i, v) in r.bands[0].data.iter().enumerate() {
                let (x, y) = ((i % w as usize) as i64, (i / w as usize) as i64);
                let want = if v.is_nan() { None } else { Some(*v) };
                assert_eq!(back.value(0, x, y), want, "{} at ({x}, {y})", r.data_type);
            }
        }
    }

    /// The untiled writer's strips take the same predictors: floats (NaN, negative and very large
    /// values) the floating-point one, RGB bytes differences, every band of `write_all`'s float
    /// samples (colour, alpha and one more) the floating-point one, and a band of a few classes
    /// none, though it is written as floats. Each image runs over more than one strip, and every value of every band reads back
    /// exactly.
    #[test]
    fn untiled_strips_read_back_exactly_through_their_predictor() {
        let (w, h) = (700u32, 600u32);
        let n = (w * h) as usize;
        let floats = |k: usize| -> Vec<f32> {
            (0..n)
                .map(|i| if i % 97 == k { f32::NAN } else { (i as f32 - 1000.0) * 3.7e-4 * if i % 5 == 0 { 1e30 } else { 1.0 } })
                .collect()
        };
        let bytes = |k: usize| -> Vec<f32> { (0..n).map(|i| ((i * 7 + k * 31) % 256) as f32).collect() };
        let classes = || -> Vec<f32> { (0..n).map(|i| (i / 100 % 3) as f32).collect() };
        let five: Vec<Vec<f32>> = (0..5).map(floats).collect();
        let cases = [
            (geotiff::write(&utm(w, h, vec![floats(0)], ColorHint::Gray, "Float32", None), 0).unwrap(), 3, 1),
            (geotiff::write(&utm(w, h, (0..3).map(bytes).collect(), ColorHint::Rgb { alpha: false }, "UInt8", None), 0).unwrap(), 2, 3),
            (geotiff::write_all(&utm(w, h, five, ColorHint::Rgb { alpha: true }, "Float32", None)).unwrap(), 3, 5),
            (geotiff::write(&utm(w, h, vec![classes()], ColorHint::Gray, "Float32", None), 0).unwrap(), 1, 1),
        ];
        for (i, (file, predictor, bands)) in cases.into_iter().enumerate() {
            let mut dec = Decoder::new(Cursor::new(file.as_slice())).unwrap();
            assert_eq!(dec.find_tag_unsigned::<u16>(Tag::Predictor).unwrap(), Some(predictor), "case {i}");
            assert!(dec.find_tag_unsigned_vec::<u64>(Tag::StripOffsets).unwrap().unwrap().len() > 1, "case {i}");
            let back = geotiff::read(Cursor::new(file), "back").unwrap();
            assert_eq!(back.band_count(), bands, "case {i}");
            let want: Vec<Vec<f32>> = match i {
                0 => vec![floats(0)],
                1 => (0..3).map(bytes).collect(),
                2 => (0..5).map(floats).collect(),
                _ => vec![classes()],
            };
            for (b, data) in want.iter().enumerate() {
                for (j, v) in data.iter().enumerate() {
                    let (x, y) = ((j % w as usize) as i64, (j / w as usize) as i64);
                    let got = back.bands[b].data[j];
                    assert!(got == *v || (got.is_nan() && v.is_nan()), "case {i} band {b} at ({x}, {y}): {got} for {v}");
                }
            }
        }
    }

    #[test]
    fn sixteen_bit_and_grey_alpha_keep_their_type() {
        let (w, h) = (600u32, 10u32);
        let n = (w * h) as usize;
        let bands: Vec<Vec<f32>> =
            (0..4).map(|b| (0..n).map(|i| if b == 3 { 255.0 } else { (i % 600) as f32 * 100.0 }).collect()).collect();
        let r = utm(w, h, bands, ColorHint::Rgb { alpha: true }, "UInt16", None);
        let back = geotiff::read(Cursor::new(write(&r, 0).unwrap()), "b").unwrap();
        assert_eq!((back.data_type.as_str(), back.band_count(), back.value(0, 599, 0)), ("UInt16", 4, Some(59_900.0)));
        // Grey with alpha keeps both bands, as bytes.
        let g = utm(2, 1, vec![vec![10.0, 20.0], vec![255.0, 0.0]], ColorHint::GrayAlpha, "UInt8", None);
        let back = geotiff::read(Cursor::new(write(&g, 0).unwrap()), "b").unwrap();
        assert_eq!((back.data_type.as_str(), back.band_count(), back.color.clone()), ("UInt8", 2, ColorHint::GrayAlpha));
        assert_eq!((back.value(1, 0, 0), back.value(1, 1, 0)), (Some(255.0), Some(0.0)));
        // A second band whose extra sample is 0, unspecified data (what GDAL writes on a plain
        // two-band file), is a band, not alpha.
        let mut bytes = write(&g, 0).unwrap();
        let entry = [0x52, 0x01, 3, 0, 1, 0, 0, 0, 2, 0];
        let at = bytes.windows(entry.len()).position(|w| w == entry).unwrap();
        bytes[at + 8] = 0;
        let back = geotiff::read(Cursor::new(bytes), "b").unwrap();
        assert_eq!((back.band_count(), back.color.clone()), (2, ColorHint::Gray));
        // With alpha, a channel equal to the no-data value is a colour: the pixel stays opaque, and
        // overviews average it.
        let bands = vec![vec![0.0; n], vec![100.0; n], vec![200.0; n], vec![255.0; n]];
        let z = utm(w, h, bands, ColorHint::Rgb { alpha: true }, "UInt8", Some(0.0));
        let bytes = write(&z, 0).unwrap();
        // No no-data tag either, so a reader doesn't take the black red channel for an empty one.
        let back = geotiff::read(Cursor::new(bytes.as_slice()), "b").unwrap();
        assert_eq!((back.nodata, back.value(0, 0, 0)), (None, Some(0.0)));
        let mut dec = Decoder::new(Cursor::new(bytes.as_slice())).unwrap();
        let DecodingResult::U8(px) = dec.read_image().unwrap() else { panic!("bytes") };
        assert_eq!(px.get(..4), Some(&[0, 100, 200, 255][..]));
        dec.seek_to_image(1).unwrap();
        let DecodingResult::U8(px) = dec.read_image().unwrap() else { panic!("bytes") };
        assert_eq!(px.get(..4), Some(&[0, 100, 200, 255][..]));
        // Byte colours with a fractional alpha keep the alpha, as floats, rather than dropping it.
        let f = utm(
            2,
            1,
            vec![vec![10.0, 20.0], vec![30.0, 40.0], vec![50.0, 60.0], vec![0.5, 1.0]],
            ColorHint::Rgb { alpha: true },
            "Float32",
            None,
        );
        let back = geotiff::read(Cursor::new(write(&f, 0).unwrap()), "b").unwrap();
        assert_eq!((back.band_count(), back.value(3, 0, 0), back.value(0, 1, 0)), (4, Some(0.5), Some(20.0)));
    }

    #[test]
    fn pixels_with_no_value_stay_without_one() {
        let nan = f32::NAN;
        // Colour bytes with a hole and no alpha or no-data: floats, so the hole is still one.
        let rgb = utm(2, 1, vec![vec![10.0, nan], vec![20.0, nan], vec![30.0, nan]], ColorHint::Rgb { alpha: false }, "UInt8", None);
        let back = geotiff::read(Cursor::new(write(&rgb, 0).unwrap()), "b").unwrap();
        assert_eq!((back.value(0, 0, 0), back.value(0, 1, 0)), (Some(10.0), None));
        // With alpha, a hole in a colour is see-through, and the file stays bytes.
        let rgba = utm(
            2,
            1,
            vec![vec![10.0, nan], vec![20.0, 5.0], vec![30.0, 5.0], vec![255.0, 255.0]],
            ColorHint::Rgb { alpha: true },
            "UInt8",
            None,
        );
        let back = geotiff::read(Cursor::new(write(&rgba, 0).unwrap()), "b").unwrap();
        assert_eq!((back.data_type.as_str(), back.value(3, 0, 0), back.value(3, 1, 0)), ("UInt8", Some(255.0), Some(0.0)));
        // A fourth band the raster doesn't call alpha isn't written as one.
        let rgbi = utm(1, 1, vec![vec![10.0], vec![20.0], vec![30.0], vec![0.0]], ColorHint::Rgb { alpha: false }, "UInt8", None);
        let back = geotiff::read(Cursor::new(write(&rgbi, 0).unwrap()), "b").unwrap();
        assert_eq!((back.band_count(), back.color), (4, ColorHint::Rgb { alpha: false }));
        // Every palette index used and a cell with none: no index is left to mark it, so refused.
        let colors: Vec<[u8; 4]> = (0..=255u8).map(|i| [i, 0, 0, 255]).collect();
        let mut idx: Vec<f32> = (0..=255u8).map(f32::from).collect();
        idx.push(nan);
        let full = utm(257, 1, vec![idx], ColorHint::Palette(colors.clone()), "UInt8", None);
        assert!(write(&full, 0).is_err());
        // An index that isn't a whole number isn't a class, even in a band called UInt8.
        let half = utm(2, 1, vec![vec![1.0, 1.5]], ColorHint::Palette(colors), "UInt8", None);
        assert!(write(&half, 0).is_err());
    }

    #[test]
    fn a_scan_is_not_written() {
        let scan = utm(2, 1, vec![vec![1.0, 2.0]], ColorHint::Gray, "UInt8", None).into_scan().unwrap();
        let e = write(&scan, 0).unwrap_err().to_string();
        assert!(e.contains("no coordinates"), "{e}");
    }

    #[test]
    fn rgbi_writers_keep_nir_without_marking_it_as_alpha() {
        for (dt, factor) in [("UInt8", 1.0), ("UInt16", 100.0), ("Float32", 0.01)] {
            let mut bands: Vec<Vec<f32>> = [200.0, 100.0, 50.0, 0.0].iter().map(|v| vec![v * factor; 16]).collect();
            // Measured NIR next to zeros: a writer that zeroes the band fails.
            if let Some(v) = bands[3].first_mut() {
                *v = 75.0 * factor;
            }
            let r = utm(4, 4, bands, ColorHint::Rgb { alpha: false }, dt, None);
            for bytes in [geotiff::write(&r, 0).unwrap(), write(&r, 0).unwrap()] {
                let back = geotiff::read(Cursor::new(bytes), "rgbi").unwrap();
                assert_eq!(back.band_count(), 4);
                assert_eq!(back.color, ColorHint::Rgb { alpha: false });
                for (b, original) in r.bands.iter().enumerate() {
                    assert_eq!(back.bands[b].data, original.data, "{dt}, band {b}");
                }
            }
            assert_eq!(written_bands(&r, 0).unwrap(), vec![0, 1, 2, 3]);
        }
    }

    #[test]
    fn the_bands_said_written_are_the_bands_written() {
        // Grey (band 2 chosen), grey with alpha, RGB bytes and RGBA floats: what each writer says it
        // writes, which the export note counts, is what it writes.
        let n = 16;
        let grey = |b: usize| vec![0.5 + b as f32; n];
        let bytes = |b: usize| vec![(10 * b) as f32; n];
        let cases = [
            (utm(4, 4, (0..3).map(grey).collect(), ColorHint::Gray, "Float32", None), 1),
            (utm(4, 4, (0..2).map(grey).collect(), ColorHint::GrayAlpha, "Float32", None), 0),
            (utm(4, 4, (0..3).map(bytes).collect(), ColorHint::Rgb { alpha: false }, "UInt8", None), 0),
            (utm(4, 4, (0..4).map(grey).collect(), ColorHint::Rgb { alpha: true }, "Float32", None), 0),
        ];
        for (i, (r, band)) in cases.iter().enumerate() {
            let back = |bytes: Vec<u8>| geotiff::read(Cursor::new(bytes), "back").unwrap().band_count();
            assert_eq!(geotiff::written_bands(r, *band).len(), back(geotiff::write(r, *band).unwrap()), "GeoTIFF, case {i}");
            assert_eq!(written_bands(r, *band).unwrap().len(), back(write(r, *band).unwrap()), "COG, case {i}");
        }
        // Grey with alpha: the COG keeps both bands, the GeoTIFF the chosen one.
        let (alpha, _) = &cases[1];
        assert_eq!((geotiff::written_bands(alpha, 0), written_bands(alpha, 0).unwrap()), (vec![0], vec![0, 1]));
    }

    /// A cancelled GeoTIFF or COG write stops with [`DataError::Cancelled`], the COG leaving no
    /// file; without a probe both are written, and the GeoTIFF writer counts its strips.
    #[test]
    fn a_cancelled_raster_write_stops() {
        use crate::progress::{install, tests::Counted};
        let (w, h) = (1100u32, 1100u32);
        let r = utm(w, h, vec![(0..w * h).map(|i| (i % 997) as f32).collect()], ColorHint::Gray, "Float32", None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.tif");
        {
            let _g = install(Some(Counted::cancelling_after(2)));
            assert!(matches!(write_file(&r, 0, &path), Err(DataError::Cancelled)));
            // Cancelled before its first strip.
            let _g = install(Some(Counted::cancelling_after(0)));
            assert!(matches!(geotiff::write(&r, 0), Err(DataError::Cancelled)));
        }
        // The GeoTIFF writer counts its strips: 1100 rows of 4400 bytes, 238 to a strip.
        let counted = std::sync::Arc::new(Counted::default());
        {
            let _g = install(Some(counted.clone()));
            assert!(geotiff::write(&r, 0).is_ok());
        }
        use std::sync::atomic::Ordering;
        assert_eq!((counted.done.load(Ordering::Relaxed), counted.total.load(Ordering::Relaxed)), (5, 5));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        write_file(&r, 0, &path).unwrap();
        assert!(geotiff::write(&r, 0).is_ok());
    }

    /// Pieces made on several threads, the later ones sooner, are written in their order; the
    /// first failure in that order comes back, and a piece that panics is an error, not a crash.
    #[test]
    fn pieces_made_on_threads_are_written_in_order() {
        let slow_first = |_: &mut (), k: usize| {
            std::thread::sleep(std::time::Duration::from_millis(((40 - k) % 7) as u64)); // test-wait: finish pieces out of order to exercise ordered writing
            Ok(vec![k as u8])
        };
        let mut written = Vec::new();
        in_order_parallel(
            40,
            4,
            err,
            || (),
            slow_first,
            |k, p| {
                written.push((k, p));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(written, (0..40).map(|k| (k, vec![k as u8])).collect::<Vec<_>>());

        let failing = |_: &mut (), k: usize| match k {
            9 => Err(err("nine")),
            17 => panic!("seventeen"),
            _ => Ok(Vec::new()),
        };
        let mut n = 0;
        let e = in_order_parallel(
            40,
            4,
            err,
            || (),
            failing,
            |_, _| {
                n += 1;
                Ok(())
            },
        );
        assert!(e.unwrap_err().to_string().contains("nine"));
        assert_eq!(n, 9, "the pieces before the failure are written");
        let panics = |_: &mut (), k: usize| if k == 3 { panic!("three") } else { Ok(Vec::new()) };
        let e = in_order_parallel(10, 3, err, || (), panics, |_, _| Ok(()));
        assert!(e.unwrap_err().to_string().contains("couldn't be made"));
        // A panic while a piece is written, with the threads waiting for it, is passed on rather
        // than leaving the write waiting for them.
        let caught_write = std::panic::catch_unwind(|| {
            in_order_parallel(40, 2, err, || (), |_, _| Ok(Vec::new()), |k, _| if k == 0 { panic!("write") } else { Ok(()) })
        });
        assert!(caught_write.is_err());
        // On one thread too (the browser, a one-core machine).
        let e = one_by_one(10, err, || (), panics, |_, _| Ok(()));
        assert!(e.unwrap_err().to_string().contains("couldn't be made"));
    }

    /// A cancel that comes while the untiled writer makes its last strip stops the write, rather
    /// than the file being finished as if nothing had happened.
    #[test]
    fn a_cancel_during_the_last_strip_stops_the_untiled_write() {
        use crate::progress::{install, tests::Counted};
        use std::sync::atomic::Ordering;
        let probe = std::sync::Arc::new(Counted::default());
        let _g = install(Some(probe.clone()));
        let img = Strips { w: 4, h: 3, spp: 1, sample: Sample::Byte, photometric: 1, extra: Vec::new(), classes: false, tags: Vec::new() };
        let res = write_strips(img, &mut Cursor::new(Vec::new()), |rows, buf| {
            if rows.end == 3 {
                probe.cancel.store(true, Ordering::Relaxed);
            }
            buf.extend(std::iter::repeat_n(7u8, rows.len() * 4));
            Ok(())
        });
        assert!(matches!(res, Err(DataError::Cancelled)));
    }
}
