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
//! Memory: the raster is read in place. A tile is gathered from the bands, compressed and written
//! before the next one, so no second copy of the pixels is made, and each overview tile is made
//! straight from the full image (a block of 2ⁿ × 2ⁿ pixels), so no pyramid is kept. The price is
//! time: every overview level reads the whole full image again, so writing takes about as many
//! passes over the pixels as there are levels, where building each level from the one before
//! would take about 1⅓ but hold a second copy.
//!
//! Classic TIFF is used when the file can't pass 4 GiB even with every tile left uncompressed,
//! BigTIFF otherwise.

use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use super::geotiff::{GeoKeys, fits_bytes, geo_keys};
use super::{ColorHint, Raster};
use crate::{DataError, Result};

/// Pixels along a tile's sides, in the full image and in every overview.
pub const TILE: u32 = 512;
/// The most images (full and overviews) written; 2³¹ pixels across needs far fewer.
const MAX_LEVELS: usize = 32;
/// Deflate effort (GDAL's default).
const DEFLATE_LEVEL: u8 = 6;

fn err(e: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("COG: {e}"))
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sample {
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

/// Chooses the file's samples like [`super::geotiff::write`] does: a colour raster's colour bands
/// (as bytes when they are bytes, alpha kept), otherwise `band` alone. 16-bit rasters stay 16-bit,
/// and a grey byte raster is written as bytes when no pixel needs a no-data value the bytes can't
/// hold. Grey with alpha keeps both bands; a palette raster keeps its colour table.
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
        if let Some(sample) = sample_of(&[0, 1, 2]) {
            let alpha = has_alpha && sample_of(&[0, 1, 2, 3]) == Some(sample);
            let max = if sample == Sample::Byte { 255.0 } else { 65_535.0 };
            // Pixels with no value need a way to say so: alpha (see-through, in tile_samples), or a
            // no-data value the samples hold. Otherwise they are written as floats, with NaN.
            let n = r.width as usize * r.height as usize;
            let holes = (0..3).any(|b| r.bands.get(b).is_some_and(|band| band.stats.count < n));
            // An alpha band that can't share the colour bands' type keeps its place through the
            // float plan below rather than being dropped.
            if alpha == has_alpha && (alpha || !holes || int_nodata(max).is_some()) {
                return Ok(Plan {
                    bands: (0..if alpha { 4 } else { 3 }).collect(),
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
        let n = if alpha { 4 } else { 3 };
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
    let categorical = sample != Sample::Float && b.stats.categories.as_ref().is_some_and(|c| c.len() <= 64);
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

// TIFF field types.
const ASCII: u16 = 2;
const SHORT: u16 = 3;
const LONG: u16 = 4;
const DOUBLE: u16 = 12;
const LONG8: u16 = 16;

/// A directory entry; `patch` marks a tile table, filled in after the tiles are written.
struct Entry {
    tag: u16,
    ty: u16,
    count: u64,
    data: Vec<u8>,
    patch: Option<bool>,
}

fn shorts(tag: u16, v: &[u16]) -> Entry {
    Entry { tag, ty: SHORT, count: v.len() as u64, data: v.iter().flat_map(|x| x.to_le_bytes()).collect(), patch: None }
}

fn long(tag: u16, v: u32) -> Entry {
    Entry { tag, ty: LONG, count: 1, data: v.to_le_bytes().to_vec(), patch: None }
}

fn doubles(tag: u16, v: &[f64]) -> Entry {
    Entry { tag, ty: DOUBLE, count: v.len() as u64, data: v.iter().flat_map(|x| x.to_le_bytes()).collect(), patch: None }
}

fn ascii(tag: u16, s: &str) -> Entry {
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
    let (bits, format) = match plan.sample {
        Sample::Byte => (8u16, 1u16),
        Sample::Short => (16, 1),
        Sample::Float => (32, 3),
    };
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
    if let Some(colors) = &plan.palette {
        // Reds, then greens, then blues, 256 of each, as 16-bit intensities.
        let map: Vec<u16> = (0..3).flat_map(|c| (0..256).map(move |i| colors.get(i).map_or(0, |rgba| u16::from(rgba[c]) * 257))).collect();
        e.push(shorts(320, &map));
    }
    e.push(long(322, TILE));
    e.push(long(323, TILE));
    e.push(table(324, lv.tiles(), big, false));
    e.push(table(325, lv.tiles(), big, true));
    if plan.alpha.is_some() {
        e.push(shorts(338, &[2]));
    }
    e.push(shorts(339, &vec![format; spp]));
    if first {
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
        // The written bands' names and the first one's class names, so a reopened window keeps
        // them (a band keeping its number has no name to write).
        if let Some(xml) = super::gdal_metadata_of(r, &plan.bands) {
            e.push(ascii(42112, &xml));
        }
        if let Some(nd) = nodata {
            e.push(ascii(42113, &nd));
        }
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
    let mut places: Vec<Vec<(u64, u64)>> = vec![Vec::new(); levels.len()];
    for (i, lv) in levels.iter().enumerate().rev() {
        for ty in 0..lv.down {
            for tx in 0..lv.across {
                let zipped = miniz_oxide::deflate::compress_to_vec_zlib(&tile_samples(r, &plan, lv, tx, ty), DEFLATE_LEVEL);
                if !big && pos + zipped.len() as u64 > u64::from(u32::MAX) {
                    return Err(err("the file is too large for classic TIFF"));
                }
                out.write_all(&zipped).map_err(err)?;
                if let Some(p) = places.get_mut(i) {
                    p.push((pos, zipped.len() as u64));
                }
                pos += zipped.len() as u64;
            }
        }
    }

    for p in &head.patches {
        let Some(tiles) = places.get(p.level) else { continue };
        let mut bytes = Vec::with_capacity(tiles.len() * 8);
        for (off, len) in tiles {
            let v = if p.counts { *len } else { *off };
            if big {
                bytes.extend_from_slice(&v.to_le_bytes());
            } else {
                bytes.extend_from_slice(&u32::try_from(v).map_err(err)?.to_le_bytes());
            }
        }
        out.seek(SeekFrom::Start(p.at)).map_err(err)?;
        out.write_all(&bytes).map_err(err)?;
    }
    out.seek(SeekFrom::Start(pos)).map_err(err)?;
    Ok(CogInfo { big, levels: levels.iter().map(|l| (l.w, l.h)).collect(), bytes: pos })
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
        std::fs::rename(&tmp, path)?;
        Ok(info)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
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
        assert_eq!(geotiff::read(Cursor::new(write(&rgbi, 0).unwrap()), "b").unwrap().band_count(), 3);
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
}
