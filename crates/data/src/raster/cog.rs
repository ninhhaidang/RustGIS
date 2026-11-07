//! Cloud-optimised GeoTIFFs read in place: the layout of a tiled GeoTIFF (its images, their tile
//! grids and where each tile is) from the bytes read so far, and one tile at a time decoded from
//! its own bytes. Nothing here fetches: [`Sparse`] holds the byte ranges the caller has read, and a
//! read past them says which range is needed next (the tile cache fetches it, by HTTP range).
//!
//! Files are hostile input: image counts, tile counts and sizes are bounded, and a tile is decoded
//! with the same limits as a whole GeoTIFF.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

use super::{ColorHint, Crs, GeoTransform};
use crate::{DataError, Result};

/// The most images (the full one and its overviews) looked at.
const MAX_LEVELS: usize = 32;
/// The most tiles an image may have.
const MAX_TILES: usize = 4 << 20;
/// How many tiles all the levels hold together (their offsets and lengths are kept, 16 bytes a
/// tile): about 128 MB at most.
const MAX_TOTAL_TILES: usize = 8 << 20;
/// The largest tile, in samples.
const MAX_TILE_SAMPLES: usize = 4 << 20;
/// The most bytes held from a file's header (its IFDs and their tag arrays).
pub const MAX_HEADER: u64 = 32 << 20;

/// Byte ranges read from a file, as a reader: a read outside them fails and says what to read.
#[derive(Clone, Default)]
pub struct Sparse {
    chunks: Arc<BTreeMap<u64, Arc<Vec<u8>>>>,
    pos: u64,
    /// The first range a read needed and didn't have.
    missing: Arc<Mutex<Option<(u64, u64)>>>,
    /// The file's length, when known: reads past it find its end, not a gap.
    len: Option<u64>,
}

/// What a container of a file's parts ([`Sparse::container`]) starts with.
const CONTAINER_MAGIC: &[u8; 16] = b"RustGIS parts 1\n";
/// The most parts a container may hold.
const MAX_PARTS: u64 = 1 << 20;

impl Sparse {
    /// The bytes read so far, starting at `at`.
    pub fn with(mut self, at: u64, bytes: Vec<u8>) -> Sparse {
        Arc::make_mut(&mut self.chunks).insert(at, Arc::new(bytes));
        self
    }

    /// The same bytes, of a file `len` bytes long.
    pub fn with_len(mut self, len: u64) -> Sparse {
        self.len = Some(len);
        self
    }

    /// Whether the file's length is known ([`Sparse::with_len`]).
    pub fn len_known(&self) -> bool {
        self.len.is_some()
    }

    /// Where the next part held after `at` starts, if any.
    pub fn next_start(&self, at: u64) -> Option<u64> {
        self.chunks.range(at.saturating_add(1)..).next().map(|(a, _)| *a)
    }

    /// The parts held, as one file: the file's length, then each part's start, length and bytes
    /// (read by [`Sparse::from_container`]). A browser keeps the parts of a large GeoTIFF it read
    /// so, as a raster layer's file and in a saved project, rather than the whole file.
    pub fn container(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.chunks.values().map(|c| c.len() + 16).sum::<usize>());
        out.extend_from_slice(CONTAINER_MAGIC);
        out.extend_from_slice(&self.len.unwrap_or(u64::MAX).to_le_bytes());
        out.extend_from_slice(&(self.chunks.len() as u64).to_le_bytes());
        for (at, c) in self.chunks.iter() {
            out.extend_from_slice(&at.to_le_bytes());
            out.extend_from_slice(&(c.len() as u64).to_le_bytes());
            out.extend_from_slice(c);
        }
        out
    }

    /// Whether `bytes` are a container of a file's parts.
    pub fn is_container(bytes: &[u8]) -> bool {
        bytes.starts_with(CONTAINER_MAGIC)
    }

    /// The parts a container holds; `None` for anything else or a damaged one.
    pub fn from_container(bytes: &[u8]) -> Option<Sparse> {
        fn u64_(rest: &mut &[u8]) -> Option<u64> {
            let (head, tail) = rest.split_first_chunk::<8>()?;
            *rest = tail;
            Some(u64::from_le_bytes(*head))
        }
        let mut rest = bytes.strip_prefix(CONTAINER_MAGIC.as_slice())?;
        let len = u64_(&mut rest)?;
        let n = u64_(&mut rest)?;
        if n > MAX_PARTS {
            return None;
        }
        let mut s = Sparse::default();
        if len != u64::MAX {
            s.len = Some(len);
        }
        for _ in 0..n {
            let (at, size) = (u64_(&mut rest)?, u64_(&mut rest)?);
            // A part past the file's end (or the largest offset) is a damaged container.
            let end = at.checked_add(size)?;
            if s.len.is_some_and(|len| end > len) {
                return None;
            }
            let size = usize::try_from(size).ok()?;
            if size > rest.len() {
                return None;
            }
            let (part, tail) = rest.split_at(size);
            rest = tail;
            s = s.with(at, part.to_vec());
        }
        rest.is_empty().then_some(s)
    }

    /// How many bytes it holds.
    pub fn held(&self) -> u64 {
        self.chunks.values().map(|c| c.len() as u64).sum()
    }

    /// A reader of the same bytes from the start, with its own record of what was missing.
    pub(crate) fn reader(&self) -> Sparse {
        Sparse { chunks: self.chunks.clone(), pos: 0, missing: Arc::new(Mutex::new(None)), len: self.len }
    }

    pub(crate) fn missing(&self) -> Option<(u64, u64)> {
        *self.missing.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Read for Sparse {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() || self.len.is_some_and(|len| self.pos >= len) {
            return Ok(0);
        }
        let hit = self.chunks.range(..=self.pos).next_back().and_then(|(at, c)| {
            let off = usize::try_from(self.pos - at).ok()?;
            (off < c.len()).then(|| c.get(off..)).flatten()
        });
        match hit {
            Some(src) => {
                // Never past the file's end, whatever a part holds.
                let left = self.len.map_or(usize::MAX, |len| usize::try_from(len - self.pos).unwrap_or(usize::MAX));
                let n = src.len().min(buf.len()).min(left);
                buf[..n].copy_from_slice(&src[..n]);
                self.pos = self.pos.saturating_add(n as u64);
                Ok(n)
            }
            None => {
                let mut m = self.missing.lock().unwrap_or_else(|e| e.into_inner());
                if m.is_none() {
                    let end = self.pos.saturating_add(buf.len() as u64);
                    *m = Some((self.pos, self.len.map_or(end, |len| end.min(len))));
                }
                Err(std::io::Error::other("rustgis: bytes not read yet"))
            }
        }
    }
}

impl Seek for Sparse {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(p) => p,
            SeekFrom::Current(d) => self.pos.checked_add_signed(d).ok_or_else(|| std::io::Error::other("seek before the start"))?,
            // The file's length isn't known: the end of what was read stands for it.
            SeekFrom::End(d) => {
                let end = self
                    .len
                    .unwrap_or_else(|| self.chunks.iter().next_back().map(|(at, c)| at.saturating_add(c.len() as u64)).unwrap_or(0));
                end.checked_add_signed(d).ok_or_else(|| std::io::Error::other("seek before the start"))?
            }
        };
        Ok(self.pos)
    }
}

/// One image of the file: the full one or an overview.
#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    /// Its place in the file's chain of images.
    pub ifd: usize,
    pub width: u32,
    pub height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    /// Where each tile is, and how many bytes it has (row by row).
    pub offsets: Vec<u64>,
    pub counts: Vec<u64>,
}

impl Level {
    pub fn across(&self) -> u32 {
        self.width.div_ceil(self.tile_width.max(1))
    }
    pub fn down(&self) -> u32 {
        self.height.div_ceil(self.tile_height.max(1))
    }
    /// How many tiles one band plane (or the whole image, bands together) has.
    pub fn tiles(&self) -> u32 {
        self.across().saturating_mul(self.down())
    }
}

/// What a tiled GeoTIFF holds and where.
#[derive(Clone, Debug)]
pub struct Layout {
    /// The full image first, then its overviews from finest to coarsest.
    pub levels: Vec<Level>,
    pub crs: Crs,
    /// The full image's georeferencing.
    pub transform: GeoTransform,
    pub nodata: Option<f64>,
    /// The no-data value as the file writes it, whose samples aren't counted as rounded.
    pub(crate) nodata_exact: Option<super::Exact>,
    pub bands: usize,
    pub color: ColorHint,
    pub data_type: &'static str,
    /// The bands are stored apart (planar configuration 2): each tile's bands are chunks of their
    /// own, band `b` of tile `i` at chunk `b × tiles + i`, so a band is read without the others.
    pub planar: bool,
    /// The colour table of a palette image (its pixels are indexes): kept apart from `color`,
    /// which says how its tiles are drawn (grey), for a window read as a raster (#1204).
    pub palette: Option<Vec<[u8; 4]>>,
    /// What GDAL's metadata tag (`GDAL_METADATA`, 42112) says about the bands: their names, and
    /// the class names of a categorical file.
    pub described: Option<super::pam::Pam>,
    /// What the header holds but isn't read: a colour table or GDAL metadata that doesn't decode,
    /// a colour table too short for its pixels. One line each, for a window to say (AGENTS.md,
    /// Never silent); drawing in place shows the file grey without them, as before.
    pub lost: Vec<String>,
}

impl Layout {
    /// A level's georeferencing: the full image's, with pixels as much larger as the level is
    /// smaller.
    pub fn transform_of(&self, level: &Level) -> GeoTransform {
        let full = self.levels.first();
        let (fw, fh) = full.map(|l| (l.width, l.height)).unwrap_or((level.width, level.height));
        GeoTransform {
            pixel_width: self.transform.pixel_width * f64::from(fw) / f64::from(level.width.max(1)),
            pixel_height: self.transform.pixel_height * f64::from(fh) / f64::from(level.height.max(1)),
            ..self.transform
        }
    }

    /// How many bands one chunk (a tile's bytes) holds: one when they are stored apart.
    pub fn chunk_bands(&self) -> usize {
        if self.planar { 1 } else { self.bands.max(1) }
    }

    /// The chunk holding band `band` (0-based) of tile `tile` of a level, and that band's place in
    /// it; `None` past the bands or tiles.
    pub fn chunk(&self, level: &Level, tile: u32, band: usize) -> Option<(u32, usize)> {
        if band >= self.bands.max(1) || tile >= level.tiles() {
            return None;
        }
        if self.planar {
            let b = u32::try_from(band).ok()?;
            Some((b.checked_mul(level.tiles())?.checked_add(tile)?, 0))
        } else {
            Some((tile, band))
        }
    }
}

/// What reading the layout came to.
#[derive(Debug)]
pub enum Parse {
    Done(Box<Layout>),
    /// These bytes (first, end) must be read first.
    Need(u64, u64),
    /// Not a tiled GeoTIFF RustGIS can read a tile at a time (strips, separate band planes…): read
    /// it whole instead.
    NotTiled(String),
}

fn err(e: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("GeoTIFF: {e}"))
}

fn limits() -> Limits {
    let mut l = Limits::default();
    l.decoding_buffer_size = MAX_TILE_SAMPLES * 8;
    l.intermediate_buffer_size = 64 << 20;
    l.ifd_value_size = 16 << 20;
    l
}

/// Whether `bytes` start a TIFF (classic or BigTIFF).
pub fn is_tiff(bytes: &[u8]) -> bool {
    matches!(bytes.get(..4), Some([b'I', b'I', 42 | 43, 0] | [b'M', b'M', 0, 42 | 43]))
}

/// The layout of a tiled GeoTIFF, from the bytes read so far.
/// Takes a level's `n` tiles from what all the levels may hold together.
fn take_tiles(allowance: &mut usize, n: usize) -> Result<()> {
    if n > MAX_TILES || n > *allowance {
        return Err(err(format!("{n} tiles is too many")));
    }
    *allowance -= n;
    Ok(())
}

pub fn layout(bytes: &Sparse) -> Result<Parse> {
    let r = bytes.reader();
    let watch = r.clone();
    match read_layout(r) {
        Ok(p) => Ok(p),
        Err(e) => match watch.missing() {
            Some((a, b)) => Ok(Parse::Need(a, b)),
            None => Err(e),
        },
    }
}

fn read_layout(r: Sparse) -> Result<Parse> {
    // The same bytes, to tell a tag not read yet from one that is broken.
    let seen = r.clone();
    let rd = super::geotiff::reader(r).map_err(err)?;
    // A palette image says so (PhotometricInterpretation 3, shown to the decoder as grey): only
    // then is a colour table read, as the whole-file read does; a grey image's stray one isn't.
    // The full image's word, which the window and the drawing read.
    let palette_image = rd.first_is_palette();
    // An RGB image with more bands (shown to the decoder as grey, see `Unpaletted`): every band,
    // the first three red, green and blue.
    let wide_rgb = rd.first_is_wide_rgb();
    let mut dec = Decoder::new(rd).map_err(err)?.with_limits(limits());
    let Some(geo) = super::geotiff::cog_geo(&mut dec)? else {
        return Ok(Parse::NotTiled("a rotated image, or one without coordinates, is read whole".into()));
    };
    let ct = dec.colortype().map_err(err)?;
    let (bands, color) = match ct {
        ColorType::Gray(_) => (1, ColorHint::Gray),
        ColorType::GrayA(_) => (2, ColorHint::GrayAlpha),
        ColorType::RGB(_) => (3, ColorHint::Rgb { alpha: false }),
        ColorType::YCbCr(_) if dec.find_tag_unsigned::<u16>(Tag::Compression).ok().flatten() == Some(7) => {
            (3, ColorHint::Rgb { alpha: false })
        }
        ColorType::RGBA(_) => (4, ColorHint::Rgb { alpha: true }),
        ColorType::Multiband { num_samples, .. } if wide_rgb && num_samples >= 3 => {
            (num_samples as usize, ColorHint::Rgb { alpha: super::geotiff::extra_alpha(&mut dec) })
        }
        ColorType::Multiband { num_samples, .. } => {
            let color = super::geotiff::multiband_color(&mut dec, num_samples);
            (num_samples.max(1) as usize, color)
        }
        other => return Ok(Parse::NotTiled(format!("{other:?} images are read whole"))),
    };
    // Bands stored apart: each band of a tile is a chunk of its own.
    let planar = dec.find_tag_unsigned::<u16>(Tag::PlanarConfiguration).ok().flatten() == Some(2) && bands > 1;
    let (planes, per_chunk) = if planar { (bands, 1) } else { (1, bands) };
    let bits = dec.find_tag_unsigned_vec::<u16>(Tag::BitsPerSample).ok().flatten().and_then(|v| v.first().copied()).unwrap_or(8);
    // One value a band (the first stands for all).
    let format = dec.find_tag_unsigned_vec::<u16>(Tag::SampleFormat).ok().flatten().and_then(|v| v.first().copied()).unwrap_or(1);
    let data_type = match (format, bits) {
        (3, 64) => "Float64",
        (3, _) => "Float32",
        (2, ..=8) => "Int8",
        (2, ..=16) => "Int16",
        (2, _) => "Int32",
        (_, ..=8) => "UInt8",
        (_, ..=16) => "UInt16",
        _ => "UInt32",
    };
    // The full image's colour table and GDAL metadata. A tag whose bytes aren't read yet asks
    // for them (the error goes up as a `Need`); one that doesn't decode is left out, and said.
    let tag = |dec: &mut Decoder<_>, t: Tag| match dec.find_tag(t) {
        Ok(v) => Ok(Ok(v)),
        Err(_) if seen.missing().is_some() => Err(err("the header isn't read yet")),
        Err(e) => Ok(Err(e.to_string())),
    };
    // Another body's system is said, as when the file is read whole.
    let mut lost: Vec<String> = super::planetary::note_of(geo.0).into_iter().collect();
    let palette = match color {
        ColorHint::Gray if palette_image => match tag(&mut dec, Tag::ColorMap)? {
            Ok(None) => {
                lost.push("it is a palette image with no colour table: drawn grey".to_string());
                None
            }
            Ok(Some(v)) => match v.into_u16_vec() {
                Ok(map) => {
                    let table = colour_table(&map, bits);
                    if table.is_none() {
                        lost.push(format!(
                            "its colour table has {} entries, short of the {} a palette of {bits}-bit pixels needs: it is left out",
                            map.len(),
                            3usize << bits.min(16)
                        ));
                    }
                    table
                }
                Err(e) => {
                    lost.push(format!("its colour table doesn't decode ({e}): it is left out"));
                    None
                }
            },
            Err(e) => {
                lost.push(format!("its colour table doesn't decode ({e}): it is left out"));
                None
            }
        },
        _ => None,
    };
    let described = match tag(&mut dec, Tag::Unknown(42112))? {
        Ok(None) => None,
        Ok(Some(v)) => match v.into_string() {
            Ok(t) => super::pam::parse_gdal_metadata(&t),
            Err(e) => {
                lost.push(format!("its GDAL metadata isn't text ({e}): band and class names are left out"));
                None
            }
        },
        Err(e) => {
            lost.push(format!("its GDAL metadata doesn't decode ({e}): band and class names are left out"));
            None
        }
    };
    let mut levels = Vec::new();
    let mut index = 0usize;
    let mut allowance = MAX_TOTAL_TILES;
    // Overviews left out for a sample count other than the full image's (its tag's, which a
    // JPEG YCbCr image's decoded bands may differ from), and how many.
    let full_samples = dec.find_tag_unsigned::<u16>(Tag::SamplesPerPixel).ok().flatten().unwrap_or(1);
    let mut mismatched: Vec<String> = Vec::new();
    let mut mismatched_count = 0usize;
    loop {
        let subfile = dec.find_tag_unsigned::<u32>(Tag::NewSubfileType).ok().flatten().unwrap_or(0);
        // Transparency masks are skipped.
        if subfile & 4 == 0 {
            let Some(tw) = dec.find_tag_unsigned::<u32>(Tag::TileWidth).ok().flatten() else {
                if index == 0 {
                    return Ok(Parse::NotTiled("a GeoTIFF in strips is read whole".into()));
                }
                break;
            };
            let th = dec.find_tag_unsigned::<u32>(Tag::TileLength).ok().flatten().unwrap_or(tw);
            let (w, h) = dec.dimensions().map_err(err)?;
            if tw == 0 || th == 0 || (tw as usize).saturating_mul(th as usize).saturating_mul(per_chunk) > MAX_TILE_SAMPLES {
                return Err(err(format!("tiles of {tw} × {th} pixels")));
            }
            // Every band plane's tiles, when the bands are stored apart.
            let n = (w.div_ceil(tw) as usize).saturating_mul(h.div_ceil(th) as usize).saturating_mul(planes);
            take_tiles(&mut allowance, n)?;
            let mut offsets = dec.find_tag_unsigned_vec::<u64>(Tag::TileOffsets).map_err(err)?.unwrap_or_default();
            let mut counts = dec.find_tag_unsigned_vec::<u64>(Tag::TileByteCounts).map_err(err)?.unwrap_or_default();
            if offsets.len() < n || counts.len() < n {
                return Err(err("the tile index is shorter than the image"));
            }
            // Entries past the image's tiles aren't kept.
            offsets.truncate(n);
            counts.truncate(n);
            // An overview a little larger than the one before isn't an overview of it.
            if levels.last().is_none_or(|l: &Level| w < l.width) {
                // A level decodes as the full image does, with as many samples: an overview with
                // another count (three of an RGBI image's four) is left out, and said.
                let samples = dec.find_tag_unsigned::<u16>(Tag::SamplesPerPixel).ok().flatten().unwrap_or(1);
                if samples == full_samples {
                    levels.push(Level { ifd: index, width: w, height: h, tile_width: tw, tile_height: th, offsets, counts });
                } else {
                    mismatched_count += 1;
                    if mismatched.len() < 5 {
                        mismatched.push(format!("image {index} ({samples})"));
                    }
                }
            }
        }
        if levels.len() >= MAX_LEVELS || !dec.more_images() {
            break;
        }
        dec.next_image().map_err(err)?;
        index += 1;
    }
    if mismatched_count > 0 {
        let more = if mismatched_count > mismatched.len() { ", …" } else { "" };
        lost.push(format!(
            "{mismatched_count} overview(s) with samples per pixel other than the full image's {full_samples} are left out: {}{more}",
            mismatched.join(", ")
        ));
    }
    if levels.is_empty() {
        return Ok(Parse::NotTiled("no image".into()));
    }
    Ok(Parse::Done(Box::new(Layout {
        levels,
        crs: geo.0,
        transform: geo.1,
        nodata: geo.2.map(super::Exact::wide),
        nodata_exact: geo.2,
        bands,
        color,
        data_type,
        planar,
        palette,
        described,
        lost,
    })))
}

/// A TIFF colour map (`ColorMap`: every red, then every green, then every blue, 16 bits each) as
/// RGBA bytes: `None` when it is shorter than the `bits` it indexes.
pub(super) fn colour_table(map: &[u16], bits: u16) -> Option<Vec<[u8; 4]>> {
    let n = 1usize << bits.min(16);
    let c = |i: usize| (map.get(i).copied().unwrap_or(0) / 257) as u8;
    (map.len() >= 3 * n).then(|| (0..n).map(|i| [c(i), c(n + i), c(2 * n + i), 255]).collect())
}

/// A decoded chunk: its bands' values, and which of them `f32` rounded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tile {
    /// Each band's `tile_width × tile_height` samples, row by row (NaN past the image's edge and
    /// for no-data).
    pub bands: Vec<Vec<f32>>,
    /// For each band, a bit a sample (as `bands` lays them out) that `f32` doesn't hold exactly
    /// (32- and 64-bit whole numbers and 64-bit floats, by the rule the whole-file reader counts
    /// with): empty when none is.
    pub rounded: Vec<Vec<u64>>,
}

impl Tile {
    /// Whether sample `i` of band `b` was rounded.
    pub fn rounded_at(&self, b: usize, i: usize) -> bool {
        self.rounded.get(b).and_then(|bits| bits.get(i / 64)).is_some_and(|w| w >> (i % 64) & 1 == 1)
    }
}

impl From<Vec<Vec<f32>>> for Tile {
    fn from(bands: Vec<Vec<f32>>) -> Tile {
        Tile { bands, rounded: Vec::new() }
    }
}

impl std::ops::Deref for Tile {
    type Target = Vec<Vec<f32>>;
    fn deref(&self) -> &Vec<Vec<f32>> {
        &self.bands
    }
}

/// Decodes one chunk (a tile, or one band of a tile when the bands are stored apart, see
/// [`Layout::chunk`]): its bands, each `tile_width × tile_height` samples (row by row; NaN past
/// the image's edge). `header` holds the file's IFDs, `bytes` the chunk's own bytes.
pub fn decode_tile(header: &Sparse, layout: &Layout, level: usize, index: u32, bytes: Vec<u8>) -> Result<Vec<Vec<f32>>> {
    decode_tile_counted(header, layout, level, index, bytes).map(|t| t.bands)
}

/// [`decode_tile`], with the samples `f32` rounded marked ([`Tile::rounded`]).
pub fn decode_tile_counted(header: &Sparse, layout: &Layout, level: usize, index: u32, bytes: Vec<u8>) -> Result<Tile> {
    let l = layout.levels.get(level).ok_or_else(|| err("no such level"))?;
    let at = *l.offsets.get(index as usize).ok_or_else(|| err("no such tile"))?;
    let r = header.clone().with(at, bytes).reader();
    let mut dec = Decoder::new(super::geotiff::reader(r).map_err(err)?).map_err(err)?.with_limits(limits());
    dec.seek_to_image(l.ifd).map_err(err)?;
    let buf = super::geotiff::read_chunk(&mut dec, index)?;
    let changed = super::geotiff::rounded_bits(&buf, super::Uncounted::new(layout.nodata_exact, None));
    let values = match buf {
        DecodingResult::U8(v) => v.into_iter().map(f32::from).collect::<Vec<f32>>(),
        DecodingResult::I8(v) => v.into_iter().map(f32::from).collect(),
        DecodingResult::U16(v) => v.into_iter().map(f32::from).collect(),
        DecodingResult::I16(v) => v.into_iter().map(f32::from).collect(),
        DecodingResult::U32(v) => v.into_iter().map(|x| x as f32).collect(),
        DecodingResult::I32(v) => v.into_iter().map(|x| x as f32).collect(),
        DecodingResult::U64(v) => v.into_iter().map(|x| x as f32).collect(),
        DecodingResult::I64(v) => v.into_iter().map(|x| x as f32).collect(),
        DecodingResult::F16(v) => v.into_iter().map(f32::from).collect(),
        DecodingResult::F32(v) => v,
        DecodingResult::F64(v) => v.into_iter().map(|x| x as f32).collect(),
    };
    let (tw, th) = (l.tile_width as usize, l.tile_height as usize);
    let nb = layout.chunk_bands();
    // A band's chunk is a tile of its plane.
    let tile = if layout.planar { index % l.tiles().max(1) } else { index };
    // An edge tile may come back cropped to the image: its rows are as wide as it says.
    let (col, row) = (tile % l.across().max(1), tile / l.across().max(1));
    let rows = (l.height - (row * l.tile_height).min(l.height)).min(l.tile_height) as usize;
    let cols_cropped = (l.width - (col * l.tile_width).min(l.width)).min(l.tile_width) as usize;
    let stride = if values.len() >= tw * th * nb { tw } else { cols_cropped.max(1) };
    let mut out = vec![vec![f32::NAN; tw * th]; nb];
    let mut rounded = if changed.is_some() { vec![vec![0u64; (tw * th).div_ceil(64)]; nb] } else { Vec::new() };
    let was_rounded = |j: usize| changed.as_ref().and_then(|c| c.get(j / 64)).is_some_and(|w| w >> (j % 64) & 1 == 1);
    for y in 0..rows.min(th) {
        for x in 0..stride.min(tw) {
            let src = (y * stride + x) * nb;
            for (b, band) in out.iter_mut().enumerate() {
                if let (Some(v), Some(slot)) = (values.get(src + b), band.get_mut(y * tw + x)) {
                    *slot = *v;
                    let i = y * tw + x;
                    if was_rounded(src + b)
                        && let Some(w) = rounded.get_mut(b).and_then(|bits| bits.get_mut(i / 64))
                    {
                        *w |= 1 << (i % 64);
                    }
                }
            }
        }
    }
    if let Some(nd) = layout.nodata {
        let nd = nd as f32;
        for band in &mut out {
            for v in band.iter_mut().filter(|v| **v == nd) {
                *v = f32::NAN;
            }
        }
    }
    Ok(Tile { bands: out, rounded })
}

/// A small cloud-optimised GeoTIFF, for tests and examples: tiled, uncompressed, single-band
/// 8-bit, in longitude/latitude from 10° E, 46° N over one degree; `size` pixels a side in tiles
/// of `tile`, with overviews halving down to one tile; pixel (x, y) of the full image is
/// `(x + y) % 256`. Its IFDs come first, as a cloud-optimised file's do.
#[doc(hidden)]
pub fn example(size: u32, tile: u32) -> Vec<u8> {
    example_with(&Example { size, tile, ..Example::default() })
}

/// What [`example_with`] writes.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct Example {
    /// Pixels a side (at most 4096).
    pub size: u32,
    /// Pixels a tile side (16 to 1024).
    pub tile: u32,
    /// Bands (1 to 16): band `b` of pixel (x, y) is `(x + y + 10 b) % 256` (`% 100 − 50` in
    /// signed bytes).
    pub bands: u16,
    /// Bands stored apart (planar configuration 2).
    pub planar: bool,
    /// Signed bytes, no-data −128 (pixel (0, 0) of every band is no-data).
    pub int8: bool,
    /// Stored bottom up: a positive Y pixel size, the first row the southernmost (at 45° N).
    pub bottom_up: bool,
    /// Its PhotometricInterpretation: 1 grey, 2 RGB (the bands past the third extra samples);
    /// `classes` makes a palette image (3) whatever this says.
    pub photometric: u16,
    /// The first extra sample (the band past the colour ones) is unassociated alpha.
    pub alpha: bool,
    /// A palette image (one band of bytes) whose index `i` is the colour (i, 255 − i, 0), with
    /// classes named in GDAL's metadata as the MapBiomas files name theirs (`CLASS_3`, `CLASS_15`).
    pub classes: bool,
    /// With `classes`: a colour table of 128 colours (too short for bytes) and a class item past
    /// the values a band holds (`CLASS_70000`), for what a reader leaves out.
    pub lossy: bool,
    /// Signed 32-bit samples of 16,777,216 plus the byte value: the odd ones `f32` rounds.
    pub wide: bool,
    /// 64-bit float samples of the byte value, but 1e39 (past `f32`'s range) for 255.
    pub huge: bool,
    /// With `classes`: the full image grey (PhotometricInterpretation 1) with the colour table
    /// anyway, which isn't a palette; its overviews stay palette images.
    pub stray: bool,
    /// With `classes`: a palette image with no colour table.
    pub no_table: bool,
    /// The full image's first tile left out, as GDAL's sparse files leave out tiles of no-data
    /// (no bytes, no offset).
    pub sparse: bool,
    /// Without overviews: the full image alone (a file saved without them).
    pub no_overviews: bool,
}

impl Default for Example {
    fn default() -> Self {
        Example {
            size: 1024,
            tile: 256,
            bands: 1,
            planar: false,
            int8: false,
            bottom_up: false,
            photometric: 1,
            alpha: false,
            classes: false,
            lossy: false,
            wide: false,
            huge: false,
            stray: false,
            no_table: false,
            sparse: false,
            no_overviews: false,
        }
    }
}

/// A small cloud-optimised GeoTIFF as [`Example`] says, laid out as [`example`] lays it out.
#[doc(hidden)]
pub fn example_with(e: &Example) -> Vec<u8> {
    let (size, tile) = (e.size.clamp(1, 4096), e.tile.clamp(16, 1024));
    let nb = u32::from(e.bands.clamp(1, 16));
    let planes = if e.planar { nb } else { 1 };
    let per_chunk = nb / planes;
    let mut sizes = vec![size];
    while !e.no_overviews && *sizes.last().unwrap_or(&tile) > tile {
        sizes.push(sizes.last().unwrap_or(&tile).div_ceil(2));
    }
    let value = |x: u32, y: u32, b: u32| -> u8 {
        if e.int8 {
            if x == 0 && y == 0 { 0x80 } else { (((x + y + 10 * b) % 100) as i32 - 50) as i8 as u8 }
        } else {
            ((x + y + 10 * b) % 256) as u8
        }
    };
    // Each image's chunks, in the file's order (band planes one after another when stored apart).
    let images: Vec<Vec<Vec<u8>>> = sizes
        .iter()
        .map(|&s| {
            let k = size / s;
            let n = s.div_ceil(tile);
            let mut chunks = Vec::new();
            for plane in 0..planes {
                for i in 0..n * n {
                    let (tx, ty) = (i % n, i / n);
                    let mut c = Vec::with_capacity((tile * tile * per_chunk) as usize);
                    for p in 0..tile * tile {
                        let (x, y) = (tx * tile + p % tile, ty * tile + p / tile);
                        for b in 0..per_chunk {
                            let band = if e.planar { plane } else { b };
                            let v = if x < s && y < s { value(x * k, y * k, band) } else { 0 };
                            if e.wide {
                                c.extend_from_slice(&(16_777_216 + i32::from(v)).to_le_bytes());
                            } else if e.huge {
                                c.extend_from_slice(&(if v == 255 { 1e39 } else { f64::from(v) }).to_le_bytes());
                            } else {
                                c.push(v);
                            }
                        }
                    }
                    chunks.push(c);
                }
            }
            if e.sparse
                && k == 1
                && let Some(first) = chunks.first_mut()
            {
                first.clear();
            }
            chunks
        })
        .collect();
    // IFD entries: (tag, type, values); type 2 ascii, 3 short, 4 long, 12 double.
    enum V {
        A(Vec<u8>),
        S(Vec<u16>),
        L(Vec<u32>),
        D(Vec<f64>),
    }
    let header_len = 8u32;
    let chunk_len = tile
        * tile
        * per_chunk
        * if e.huge {
            8
        } else if e.wide {
            4
        } else {
            1
        };
    let mut ifds: Vec<Vec<(u16, V)>> = Vec::new();
    for (i, &s) in sizes.iter().enumerate() {
        let n = (s.div_ceil(tile) * s.div_ceil(tile) * planes) as usize;
        let mut t = vec![
            (254u16, V::L(vec![if i == 0 { 0 } else { 1 }])),
            (256, V::L(vec![s])),
            (257, V::L(vec![s])),
            (
                258,
                V::S(vec![
                    if e.huge {
                        64
                    } else if e.wide {
                        32
                    } else {
                        8
                    };
                    nb as usize
                ]),
            ),
            (259, V::S(vec![1])),
            (262, V::S(vec![if e.classes && !(e.stray && i == 0) { 3 } else { e.photometric }])),
            (277, V::S(vec![nb as u16])),
            (284, V::S(vec![if e.planar { 2 } else { 1 }])),
            (322, V::L(vec![tile])),
            (323, V::L(vec![tile])),
            (324, V::L(vec![0; n])),
            (325, V::L(images.get(i).map_or(vec![chunk_len; n], |c| c.iter().map(|c| c.len() as u32).collect()))),
        ];
        // The samples past the colour ones (three of an RGB image, else one): the first alpha
        // when asked, the others undefined.
        let colours = if e.photometric == 2 { 3 } else { 1 };
        if nb as usize > colours {
            let mut extra = vec![0u16; nb as usize - colours];
            if e.alpha {
                extra[0] = 2;
            }
            t.push((338, V::S(extra)));
        }
        if e.int8 || e.wide {
            t.push((339, V::S(vec![2; nb as usize])));
        }
        if e.huge {
            t.push((339, V::S(vec![3; nb as usize])));
        }
        if i == 0 {
            let px = 1.0 / f64::from(size);
            if e.bottom_up {
                t.push((34264, V::D(vec![px, 0.0, 0.0, 10.0, 0.0, px, 0.0, 45.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0])));
            } else {
                t.push((33550, V::D(vec![px, px, 0.0])));
                t.push((33922, V::D(vec![0.0, 0.0, 0.0, 10.0, 46.0, 0.0])));
            }
            t.push((34735, V::S(vec![1, 1, 0, 2, 1024, 0, 1, 2, 2048, 0, 1, 4326])));
            if e.classes {
                let xml = if e.lossy {
                    r#"<GDALMetadata><Item name="CLASS_3">Forest Formation</Item><Item name="CLASS_70000">Far</Item></GDALMetadata>"#
                } else {
                    r#"<GDALMetadata><Item name="CLASS_3">Forest Formation</Item><Item name="CLASS_15">Pasture</Item></GDALMetadata>"#
                };
                t.push((42112, V::A([xml.as_bytes(), b"\0"].concat())));
            }
        }
        if e.int8 {
            t.push((42113, V::A(b"-128\0".to_vec())));
        }
        if e.classes && !e.no_table {
            let n = if e.lossy { 128 } else { 256 };
            let red: Vec<u16> = (0..n).map(|i| i * 257).collect();
            let green: Vec<u16> = (0..n).map(|i| (255 - i) * 257).collect();
            t.push((320, V::S([red, green, vec![0; n as usize]].concat())));
        }
        t.sort_by_key(|(tag, _)| *tag);
        ifds.push(t);
    }
    // Sizes: each IFD is 2 + 12·entries + 4, its out-of-line values after it.
    let size_of = |v: &V| match v {
        V::A(x) => x.len(),
        V::S(x) => x.len() * 2,
        V::L(x) => x.len() * 4,
        V::D(x) => x.len() * 8,
    };
    let mut ifd_at = Vec::new();
    let mut at = header_len as usize;
    for t in &ifds {
        ifd_at.push(at);
        at += 2 + 12 * t.len() + 4;
        at += t.iter().map(|(_, v)| size_of(v)).filter(|s| *s > 4).map(|s| s + s % 2).sum::<usize>();
    }
    // Chunk data after every IFD.
    let mut data_at = at;
    let mut out = vec![b'I', b'I', 42, 0];
    out.extend_from_slice(&header_len.to_le_bytes());
    for (lvl, t) in ifds.iter_mut().enumerate() {
        for (tag, v) in t.iter_mut() {
            if *tag == 324
                && let V::L(offs) = v
            {
                for (j, o) in offs.iter_mut().enumerate() {
                    // A chunk left out has no offset, as GDAL writes a sparse file.
                    let len = images.get(lvl).and_then(|c| c.get(j)).map_or(0, Vec::len);
                    *o = if len == 0 { 0 } else { data_at as u32 };
                    data_at += len;
                }
            }
        }
    }
    for (i, t) in ifds.iter().enumerate() {
        // Written in order, each IFD where `ifd_at` placed it.
        let mut extra = out.len() + 2 + 12 * t.len() + 4;
        let mut body = Vec::new();
        let mut tail = Vec::new();
        body.extend_from_slice(&(t.len() as u16).to_le_bytes());
        for (tag, v) in t {
            let (ty, count, bytes): (u16, usize, Vec<u8>) = match v {
                V::A(x) => (2, x.len(), x.clone()),
                V::S(x) => (3, x.len(), x.iter().flat_map(|a| a.to_le_bytes()).collect()),
                V::L(x) => (4, x.len(), x.iter().flat_map(|a| a.to_le_bytes()).collect()),
                V::D(x) => (12, x.len(), x.iter().flat_map(|a| a.to_le_bytes()).collect()),
            };
            body.extend_from_slice(&tag.to_le_bytes());
            body.extend_from_slice(&ty.to_le_bytes());
            body.extend_from_slice(&(count as u32).to_le_bytes());
            if bytes.len() <= 4 {
                let mut b = bytes.clone();
                b.resize(4, 0);
                body.extend_from_slice(&b);
            } else {
                body.extend_from_slice(&(extra as u32).to_le_bytes());
                let mut b = bytes.clone();
                if b.len() % 2 == 1 {
                    b.push(0);
                }
                extra += b.len();
                tail.extend_from_slice(&b);
            }
        }
        let next = ifd_at.get(i + 1).copied().unwrap_or(0) as u32;
        body.extend_from_slice(&next.to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&tail);
    }
    for image in &images {
        for c in image {
            out.extend_from_slice(c);
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn tile_budget_across_levels() {
        let mut allowance = MAX_TOTAL_TILES;
        assert!(take_tiles(&mut allowance, MAX_TILES + 1).is_err());
        assert!(take_tiles(&mut allowance, MAX_TILES).is_ok() && take_tiles(&mut allowance, MAX_TILES).is_ok());
        // All the levels together past the budget: refused.
        assert!(take_tiles(&mut allowance, 1).is_err());
    }

    #[test]
    fn layout_from_the_header_then_tiles_one_by_one() {
        let file = example(1024, 256);
        // The first 64 bytes aren't enough: it says what to read next, until it has the IFDs.
        let mut s = Sparse::default().with(0, file[..64].to_vec());
        let mut rounds = 0;
        let l = loop {
            match layout(&s).unwrap() {
                Parse::Done(l) => break l,
                Parse::Need(a, b) => {
                    rounds += 1;
                    assert!(rounds < 20);
                    let end = (b.max(a + 256) as usize).min(file.len());
                    s = s.with(a, file[a as usize..end].to_vec());
                }
                Parse::NotTiled(why) => panic!("{why}"),
            }
        };
        assert_eq!(l.levels.iter().map(|l| l.width).collect::<Vec<_>>(), vec![1024, 512, 256]);
        assert_eq!(l.crs, Crs::LonLat);
        assert_eq!((l.transform.origin_x, l.transform.origin_y), (10.0, 46.0));
        let t = l.transform_of(&l.levels[2]);
        assert!((t.pixel_width - 1.0 / 256.0).abs() < 1e-12);
        // Tile 5 of the full image (row 1, column 1): pixel (256 + x) + (256 + y).
        let lv = &l.levels[0];
        let (at, n) = (lv.offsets[5] as usize, lv.counts[5] as usize);
        let bands = decode_tile(&s, &l, 0, 5, file[at..at + n].to_vec()).unwrap();
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0][0], 0.0); // (256 + 256) % 256
        assert_eq!(bands[0][3 * 256 + 2], 5.0);
        // The coarsest overview: every fourth pixel.
        let lv = &l.levels[2];
        let (at, n) = (lv.offsets[0] as usize, lv.counts[0] as usize);
        let bands = decode_tile(&s, &l, 2, 0, file[at..at + n].to_vec()).unwrap();
        assert_eq!(bands[0][256 + 1], 8.0);
        // A wrong tile index, junk tile bytes: errors, never a panic.
        assert!(decode_tile(&s, &l, 0, 99, Vec::new()).is_err());
        let _ = decode_tile(&s, &l, 0, 0, vec![1, 2, 3]);
    }

    /// Reads a file's layout from its bytes, as the tile cache would.
    fn layout_of(file: &[u8]) -> (Sparse, Box<Layout>) {
        let s = Sparse::default().with(0, file.to_vec());
        match layout(&s).unwrap() {
            Parse::Done(l) => (s, l),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn bands_stored_apart_signed_bytes_bottom_up() {
        // As AlphaEarth's embeddings are: planar, Int8 with no-data −128, a positive Y pixel size;
        // 600 pixels in tiles of 256, so the right and bottom tiles are cut by the image's edge.
        let e = Example { size: 600, tile: 256, bands: 4, planar: true, int8: true, bottom_up: true, ..Example::default() };
        let file = example_with(&e);
        let (s, l) = layout_of(&file);
        assert!(l.planar && l.bands == 4 && l.data_type == "Int8" && l.chunk_bands() == 1);
        assert_eq!(l.nodata, Some(-128.0));
        assert!(l.transform.pixel_height > 0.0 && (l.transform.origin_y - 45.0).abs() < 1e-12);
        let full = &l.levels[0];
        assert_eq!((full.tiles(), full.offsets.len()), (9, 36));
        // Band 2 of tile 4 (row 1, column 1) is chunk 2 × 9 + 4, a band of its own.
        assert_eq!(l.chunk(full, 4, 2), Some((22, 0)));
        assert_eq!(l.chunk(full, 9, 0), None);
        assert_eq!(l.chunk(full, 0, 4), None);
        let read = |level: usize, chunk: u32| {
            let lv = &l.levels[level];
            let (at, n) = (lv.offsets[chunk as usize] as usize, lv.counts[chunk as usize] as usize);
            decode_tile(&s, &l, level, chunk, file[at..at + n].to_vec()).unwrap()
        };
        let want = |x: u32, y: u32, b: u32| (((x + y + 10 * b) % 100) as i32 - 50) as f32;
        let t = read(0, 22);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0][3 * 256 + 7], want(256 + 7, 256 + 3, 2));
        // The no-data pixel (0, 0) of band 1: NaN.
        let (c, _) = l.chunk(full, 0, 1).unwrap();
        let t = read(0, c);
        assert!(t[0][0].is_nan() && t[0][1] == want(1, 0, 1));
        // An edge tile (row 2, column 2) of the last band: its pixels, and NaN past the image.
        let (c, _) = l.chunk(full, 8, 3).unwrap();
        let t = read(0, c);
        assert_eq!(t[0][5 * 256 + 9], want(512 + 9, 512 + 5, 3));
        assert!(t[0][100 * 256 + 100].is_nan());
        // Junk chunks and indices: errors, never a panic.
        assert!(decode_tile(&s, &l, 0, 36, Vec::new()).is_err());
        let _ = decode_tile(&s, &l, 0, 35, vec![9; 7]);
        // Bands together (chunky) are still read all at once.
        let (_, l2) = layout_of(&example_with(&Example { planar: false, ..e }));
        assert!(!l2.planar && l2.chunk_bands() == 4 && l2.chunk(&l2.levels[0], 4, 2) == Some((4, 2)));
        for n in (0..file.len().min(4000)).step_by(7) {
            let _ = layout(&Sparse::default().with(0, file[..n].to_vec()));
        }
    }

    #[test]
    fn a_palette_and_class_names_come_with_the_layout() {
        let file = example_with(&Example { size: 512, tile: 256, classes: true, ..Example::default() });
        let (_, l) = layout_of(&file);
        // Drawn grey as before; the table is kept apart, for a window read as a raster.
        assert!(matches!(l.color, ColorHint::Gray));
        let table = l.palette.as_ref().unwrap();
        assert_eq!((table.len(), table[3]), (256, [3, 252, 0, 255]));
        let classes = &l.described.as_ref().unwrap().band(1).unwrap().categories;
        assert_eq!((classes[3].as_str(), classes[15].as_str()), ("Forest Formation", "Pasture"));
        assert!(l.lost.is_empty(), "{:?}", l.lost);
        let (_, plain) = layout_of(&example(512, 256));
        assert!(plain.palette.is_none() && plain.described.is_none() && plain.lost.is_empty());
        assert_eq!(colour_table(&[0; 6], 8), None, "shorter than 256 colours");
        // A table too short for its pixels is left out, and said; so are odd class items.
        let (_, lossy) = layout_of(&example_with(&Example { size: 512, tile: 256, classes: true, lossy: true, ..Example::default() }));
        assert!(lossy.palette.is_none());
        assert!(lossy.lost.iter().any(|n| n.contains("384 entries, short of the 768")), "{:?}", lossy.lost);
        let described = lossy.described.as_ref().unwrap();
        assert_eq!(described.band(1).unwrap().categories[3], "Forest Formation");
        assert!(described.lost.iter().any(|n| n.contains("1 CLASS_ item(s)")), "{:?}", described.lost);
        // A grey image's stray colour table isn't a palette (as the whole-file read has it), and a
        // palette image with no table is drawn grey and says so.
        let (_, stray) = layout_of(&example_with(&Example { size: 512, tile: 256, classes: true, stray: true, ..Example::default() }));
        assert!(stray.palette.is_none() && stray.lost.is_empty(), "{:?}", stray.lost);
        // The whole-file read of that file, whose overviews are palette images, has it so too.
        let whole = super::super::read_bytes(
            "stray.tif",
            &example_with(&Example { size: 512, tile: 256, classes: true, stray: true, ..Example::default() }),
        )
        .unwrap();
        assert!(matches!(whole.color, super::super::ColorHint::Gray) && whole.lost.is_empty(), "{:?}", whole.lost);
        // So has a whole-file read that takes one of its (palette) overviews, the full image being
        // too large.
        let bytes = example_with(&Example { size: 512, tile: 256, classes: true, stray: true, ..Example::default() });
        let reader = super::super::geotiff::reader(std::io::Cursor::new(bytes.as_slice())).unwrap();
        let small = super::super::geotiff::read_limited(reader, "stray", 100_000, None).unwrap();
        assert!(small.overview.is_some(), "an overview read");
        assert!(matches!(small.color, super::super::ColorHint::Gray) && small.lost.is_empty(), "{:?}", small.lost);
        let (_, bare) = layout_of(&example_with(&Example { size: 512, tile: 256, classes: true, no_table: true, ..Example::default() }));
        assert!(bare.palette.is_none(), "{:?}", bare.palette);
        assert_eq!(bare.lost, ["it is a palette image with no colour table: drawn grey"]);
        // GDAL's metadata tag pointing past the end of a file of known length: the layout is read,
        // the tag left out and said, as the whole-file read has it.
        let mut bad = example_with(&Example { size: 512, tile: 256, classes: true, ..Example::default() });
        let n = usize::from(u16::from_le_bytes([bad[8], bad[9]]));
        let len = u32::try_from(bad.len()).unwrap();
        let entry = (0..n).map(|k| 10 + k * 12).find(|&e| bad[e..e + 2] == [0x80, 0xA4]).unwrap();
        bad[entry + 8..entry + 12].copy_from_slice(&(len + 1000).to_le_bytes());
        let Parse::Done(l) = layout(&Sparse::default().with_len(u64::from(len)).with(0, bad)).unwrap() else { panic!("not read") };
        let said: Vec<&String> = l.lost.iter().chain(l.described.iter().flat_map(|d| d.lost.iter())).collect();
        assert!(said.iter().any(|x| x.contains("band and class names are left out")), "{said:?}");
    }

    #[test]
    fn strips_and_junk_are_refused() {
        assert!(is_tiff(b"II*\0") && is_tiff(b"MM\0+") && !is_tiff(b"PK\x03\x04"));
        let strips = crate::raster::geotiff::tests_support::strip_tiff();
        assert!(matches!(layout(&Sparse::default().with(0, strips)).unwrap(), Parse::NotTiled(_)));
        // A rotated image is read whole, and turned north-up then (issue #251).
        let m = [30.0, 5.0, 0.0, 400_000.0, 3.0, -30.0, 0.0, 5_000_000.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let rotated = crate::raster::geotiff::tests_support::matrix_tiff(m, false);
        assert!(matches!(layout(&Sparse::default().with(0, rotated)).unwrap(), Parse::NotTiled(_)));
        // Every truncation of a COG: a range asked for, an error, never a panic.
        let file = example(512, 256);
        for n in 0..300 {
            let _ = layout(&Sparse::default().with(0, file[..n].to_vec()));
        }
        let _ = layout(&Sparse::default().with(0, b"II*\0\xff\xff\xff\x7f".to_vec()));
    }

    #[test]
    fn every_band_whatever_the_photometric_interpretation_says() {
        let want = |b: u32| ((300 + 10 * b) % 256) as f32;
        let e = Example { size: 300, tile: 128, ..Example::default() };
        // Grey, as remote sensing images mostly are: eight bands (a WorldView multispectral image),
        // interleaved or stored apart; a panchromatic one; a pan band with alpha.
        for planar in [false, true] {
            let (l, v) = pixel(&example_with(&Example { bands: 8, planar, ..e }), 150, 150);
            assert_eq!((l.bands, &l.color), (8, &ColorHint::Gray));
            assert_eq!(v, (0..8).map(want).collect::<Vec<_>>());
        }
        let (l, v) = pixel(&example_with(&Example { bands: 1, ..e }), 150, 150);
        assert_eq!((l.bands, &l.color, v), (1, &ColorHint::Gray, vec![want(0)]));
        let (l, v) = pixel(&example_with(&Example { bands: 2, alpha: true, ..e }), 150, 150);
        assert_eq!((l.bands, &l.color, v), (2, &ColorHint::GrayAlpha, vec![want(0), want(1)]));
        // RGB: three bands, with alpha, and with bands past the colour ones (RGB and near-infrared,
        // seven bands), which the decoder alone leaves out or refuses: every band, the first three
        // red, green and blue.
        for (bands, alpha) in [(3, false), (4, true), (4, false), (7, false), (5, true)] {
            for planar in [false, true] {
                let file = example_with(&Example { bands, alpha, planar, photometric: 2, ..e });
                let (l, v) = pixel(&file, 150, 150);
                assert_eq!((l.bands, &l.color), (usize::from(bands), &ColorHint::Rgb { alpha }), "{bands} bands, alpha {alpha}");
                assert_eq!(v, (0..u32::from(bands)).map(want).collect::<Vec<_>>(), "{bands} bands, alpha {alpha}, planar {planar}");
            }
        }
        // A palette image: its values, drawn grey, with its colour table apart.
        let (l, v) = pixel(&example_with(&Example { classes: true, ..e }), 150, 150);
        let table = l.palette.as_ref().unwrap();
        assert_eq!((&l.color, table.len(), table[44], v), (&ColorHint::Gray, 256, [44, 211, 0, 255], vec![want(0)]));
    }

    /// A GeoTIFF file is read as the same image read in place is: every band, whatever its
    /// PhotometricInterpretation says, coloured alike.
    #[test]
    fn files_have_every_band_a_cog_has() {
        let e = Example { size: 300, tile: 128, ..Example::default() };
        for (bands, alpha, photometric) in [(8, false, 1), (2, true, 1), (4, true, 2), (4, false, 2), (7, false, 2), (5, true, 2)] {
            for planar in [false, true] {
                let file = example_with(&Example { bands, alpha, planar, photometric, ..e });
                let (l, v) = pixel(&file, 150, 150);
                let r = crate::raster::geotiff::read(std::io::Cursor::new(&file), "f").unwrap();
                let read: Vec<f32> = (0..r.band_count()).map(|b| r.value(b, 150, 150).unwrap()).collect();
                assert_eq!((r.band_count(), &r.color, read), (l.bands, &l.color, v), "{bands} bands, photometric {photometric}");
            }
        }
    }

    /// A file holding an RGB image with more bands and a grey overview: the full image's word
    /// goes, whichever image is read, as a palette file's does.
    #[test]
    fn a_file_mixing_grey_and_rgb_images_goes_by_the_full_image() {
        let e = Example { size: 300, tile: 128, bands: 4, photometric: 2, ..Example::default() };
        let mut file = example_with(&e);
        // The coarsest overview's PhotometricInterpretation (tag 262, one short) from RGB to grey.
        let entry = [0x06, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00];
        let at: Vec<usize> = (0..file.len().saturating_sub(entry.len())).filter(|&i| file[i..i + entry.len()] == entry).collect();
        assert_eq!(at.len(), 3, "the full image and its overviews");
        file[at[2] + 8] = 1;
        let (l, _) = pixel(&file, 150, 150);
        assert_eq!((l.bands, &l.color), (4, &ColorHint::Rgb { alpha: false }));
        let r = crate::raster::geotiff::read(std::io::Cursor::new(&file), "mixed").unwrap();
        assert_eq!((r.band_count(), &r.color), (4, &ColorHint::Rgb { alpha: false }));
        let small = crate::raster::geotiff::read_limited(
            crate::raster::geotiff::reader(std::io::Cursor::new(&file)).unwrap(),
            "mixed",
            30_000,
            None,
        )
        .unwrap();
        assert_eq!((small.width, small.band_count(), &small.color), (75, 4, &ColorHint::Rgb { alpha: false }));
    }
    /// Pixel (x, y)'s value in every band of an example's full image, read a tile at a time.
    fn pixel(file: &[u8], x: u32, y: u32) -> (Box<Layout>, Vec<f32>) {
        let (s, l) = layout_of(file);
        let lv = &l.levels[0];
        let tile = (y / lv.tile_height) * lv.across() + x / lv.tile_width;
        let at = ((y % lv.tile_height) * lv.tile_width + x % lv.tile_width) as usize;
        let values = (0..l.bands)
            .map(|b| {
                let (chunk, i) = l.chunk(lv, tile, b).unwrap();
                let (o, n) = (lv.offsets[chunk as usize] as usize, lv.counts[chunk as usize] as usize);
                decode_tile(&s, &l, 0, chunk, file[o..o + n].to_vec()).unwrap()[i][at]
            })
            .collect();
        (l, values)
    }
}
