//! LERC-compressed TIFF strips and tiles (Compression 34887), decoded here: the `tiff` crate has no
//! LERC codec (#1818).
//!
//! GDAL writes them (`COMPRESS=LERC`, `LERC_DEFLATE`, `LERC_ZSTD`), and so do the producers that use
//! it: LINZ's elevation models, ESA's BIOMASS products. A chunk holds one LERC blob (Esri's Limited
//! Error Raster Compression), possibly wrapped in Deflate or Zstandard: the `LercParameters` tag
//! (50674) gives the LERC version and that extra layer (0 none, 1 Deflate, 2 Zstandard). A blob of
//! a chunk with several samples per pixel holds them all, pixel by pixel, or one blob per band one
//! after another, each with its own validity mask (a later band's may be left out when it is the
//! one before it). Samples a mask marks invalid are said ([`Chunk::gaps`]): the readers make them
//! no-data. A Byte image whose last sample is an unassociated alpha of only 0 and 255 is stored by
//! GDAL without that sample, its alpha the mask: it is put back, 255 where the mask is set (no mask:
//! every pixel valid, so opaque, as GDAL reads it). A chunk with no bytes (a sparse file's) is all
//! no-data. A blob must hold samples of the image's own type: another type is an error, not a cast.

use std::io::{Read, Seek, SeekFrom};

use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

use crate::{DataError, Result};

/// The TIFF compression code of LERC.
pub(crate) const COMPRESSION: u16 = 34887;

/// Most bytes in a chunk, compressed or decoded (the decoder's own intermediate limit).
pub(super) const MAX_CHUNK_BYTES: usize = 256 << 20;

fn err(e: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("GeoTIFF: LERC: {e}"))
}

/// Whether the decoder's current image is LERC-compressed.
pub(crate) fn is_lerc<R: Read + Seek>(dec: &mut Decoder<R>) -> bool {
    dec.find_tag_unsigned::<u16>(Tag::Compression).ok().flatten() == Some(COMPRESSION)
}

/// Where an image's chunks are in the file: its offsets and byte counts, read once for every chunk
/// of the image a reader decodes (the `tiff` crate keeps its own copy private).
#[derive(Default)]
pub(crate) struct Table {
    offsets: Vec<u64>,
    counts: Vec<u64>,
}

impl Table {
    /// Chunk `index`'s offset and byte count, the table read from the decoder's current image the
    /// first time.
    fn get<R: Read + Seek>(&mut self, dec: &mut Decoder<R>, tiled: bool, index: u32) -> Result<(u64, u64)> {
        if self.offsets.is_empty() {
            let (offsets, counts) = if tiled { (Tag::TileOffsets, Tag::TileByteCounts) } else { (Tag::StripOffsets, Tag::StripByteCounts) };
            self.offsets = dec.find_tag_unsigned_vec::<u64>(offsets).ok().flatten().unwrap_or_default();
            self.counts = dec.find_tag_unsigned_vec::<u64>(counts).ok().flatten().unwrap_or_default();
        }
        match (self.offsets.get(index as usize), self.counts.get(index as usize)) {
            (Some(o), Some(c)) => Ok((*o, *c)),
            _ => Err(err("no such chunk")),
        }
    }
}

/// A decoded chunk: its samples, cropped to the image as `Decoder::read_chunk` returns them, and
/// which of them the chunk's validity masks mark invalid (`None` when none is).
pub(crate) struct Chunk {
    pub(crate) samples: DecodingResult,
    pub(crate) gaps: Option<Vec<bool>>,
}

/// Chunk `index` (a strip or tile, counting through every plane) of the decoder's current image.
/// `table` keeps the image's chunk offsets from one call to the next.
pub(crate) fn chunk<R: Read + Seek>(dec: &mut Decoder<R>, index: u32, table: &mut Table) -> Result<Chunk> {
    let (w, h) = dec.dimensions().map_err(err)?;
    let tag = |dec: &mut Decoder<R>, t: Tag| dec.find_tag_unsigned::<u32>(t).ok().flatten();
    let tiled = tag(dec, Tag::TileWidth).is_some();
    let (cw, ch) = if tiled {
        (tag(dec, Tag::TileWidth).unwrap_or(0), tag(dec, Tag::TileLength).unwrap_or(0))
    } else {
        (w, tag(dec, Tag::RowsPerStrip).unwrap_or(h).min(h))
    };
    if cw == 0 || ch == 0 {
        return Err(err("a chunk has no size"));
    }
    let bits = dec.find_tag_unsigned_vec::<u16>(Tag::BitsPerSample).ok().flatten().unwrap_or_else(|| vec![8]);
    let bps = *bits.first().unwrap_or(&8);
    if bits.iter().any(|b| *b != bps) {
        return Err(err("bands of different sample sizes aren't supported"));
    }
    let format = dec.find_tag_unsigned_vec::<u16>(Tag::SampleFormat).ok().flatten().and_then(|v| v.first().copied()).unwrap_or(1);
    let spp = tag(dec, Tag::SamplesPerPixel).unwrap_or(1).clamp(1, u32::from(u16::MAX)) as usize;
    let planar = tag(dec, Tag::PlanarConfiguration) == Some(2);
    let samples = if planar { 1 } else { spp };
    let extra = dec.find_tag_unsigned_vec::<u32>(Tag::Unknown(50674)).ok().flatten().and_then(|v| v.get(1).copied()).unwrap_or(0);
    // An unassociated alpha (ExtraSamples 2) last, in a Byte image of pixels stored together: GDAL
    // may keep it as the mask.
    let alpha_last = !planar
        && samples > 1
        && bps == 8
        && format == 1
        && dec.find_tag_unsigned_vec::<u16>(Tag::ExtraSamples).ok().flatten().and_then(|v| v.last().copied()) == Some(2);
    let nodata = dec
        .find_tag(Tag::Unknown(42113))
        .ok()
        .flatten()
        .and_then(|v| v.into_string().ok())
        .and_then(|s| s.trim_matches(char::from(0)).trim().parse::<f64>().ok());

    // Where the chunk is in its plane, and how much of it is image.
    let across = w.div_ceil(cw);
    let per_plane = across.saturating_mul(h.div_ceil(ch)).max(1);
    let c = index % per_plane;
    let (x0, y0) = ((c % across.max(1)) * cw, (c / across.max(1)) * ch);
    let dw = if tiled { cw.min(w.saturating_sub(x0)) } else { w } as usize;
    let dh = ch.min(h.saturating_sub(y0)) as usize;
    // A tile is coded whole, padding included; a strip has at least the image's rows (a producer
    // may pad the last one to the full strip).
    let (bw, bh) = (cw as usize, if tiled { ch as usize } else { dh });

    let float = format == 3;
    let fill = if float { f64::NAN } else { nodata.unwrap_or(0.0) };
    // Bounded before anything is allocated, a chunk with no bytes too (8 bytes a sample, as decoded).
    let coded = bw.checked_mul(bh).and_then(|p| p.checked_mul(samples)).filter(|s| s.saturating_mul(8) <= MAX_CHUNK_BYTES);
    let Some(n) = coded.map(|_| dw * dh * samples) else {
        return Err(err("a chunk is too large"));
    };
    let expected = data_type(format, bps)?;
    let (offset, count) = table.get(dec, tiled, index)?;
    if count == 0 {
        // A chunk with no bytes (a sparse file's): all no-data, as GDAL reads it.
        let mut out = empty(format, bps, n)?;
        for _ in 0..n {
            push(&mut out, None, fill);
        }
        return Ok(Chunk { samples: out, gaps: Some(vec![true; n]) });
    }
    let count = usize::try_from(count).ok().filter(|n| *n <= MAX_CHUNK_BYTES).ok_or_else(|| err("a chunk is too large"))?;
    // Read as far as the file goes, not allocated from the claim.
    let r = dec.inner();
    r.seek(SeekFrom::Start(offset)).map_err(err)?;
    let mut encoded = Vec::new();
    r.take(count as u64).read_to_end(&mut encoded).map_err(err)?;
    if encoded.len() < count {
        return Err(err("a chunk is cut short"));
    }
    let blob = unwrap_layer(encoded, extra)?;

    let decoded = decode_blob(&blob, Shape { width: bw, height: bh, strip: !tiled, samples, alpha_last }, expected)?;
    let mut out = empty(format, bps, n)?;
    let mut gaps = decoded.valid.as_ref().map(|_| Vec::with_capacity(n));
    for y in 0..dh {
        let row = y * bw;
        for x in 0..dw {
            for s in 0..samples {
                let i = (row + x) * samples + s;
                let valid = decoded.valid.as_ref().is_none_or(|m| m.get(i).copied().unwrap_or(false));
                push(&mut out, if valid { decoded.values.get(i).copied() } else { None }, fill);
                if let Some(g) = gaps.as_mut() {
                    g.push(!valid);
                }
            }
        }
    }
    // A chunk with every sample valid says no gaps.
    let gaps = gaps.filter(|g| g.iter().any(|b| *b));
    Ok(Chunk { samples: out, gaps })
}

/// Turns the samples `gaps` marks (see [`Chunk::gaps`]) into NaN, no-data, in `values`, the chunk's
/// samples as 32-bit floats in the same order.
pub(crate) fn blank(values: &mut [f32], gaps: Option<&[bool]>) {
    for (v, g) in values.iter_mut().zip(gaps.unwrap_or_default()) {
        if *g {
            *v = f32::NAN;
        }
    }
}

/// The LERC blob inside a chunk: undoes the Deflate or Zstandard layer `extra` names.
fn unwrap_layer(encoded: Vec<u8>, extra: u32) -> Result<Vec<u8>> {
    match extra {
        0 => Ok(encoded),
        1 => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&encoded, MAX_CHUNK_BYTES)
            .map_err(|e| err(format!("its Deflate layer: {e:?}"))),
        2 => {
            let dec = ruzstd::decoding::StreamingDecoder::new(encoded.as_slice()).map_err(|e| err(format!("its Zstandard layer: {e}")))?;
            super::decoded::read(dec, MAX_CHUNK_BYTES, "GeoTIFF: LERC: its Zstandard layer")
        }
        n => Err(err(format!("extra compression {n} isn't supported"))),
    }
}

/// A chunk's decoded samples, pixel by pixel (every sample of a pixel together), and which of them
/// are valid (`None`: all of them).
struct Blobs {
    values: Vec<f64>,
    valid: Option<Vec<bool>>,
}

/// The LERC type of a TIFF's samples (`SampleFormat`, `BitsPerSample`).
fn data_type(format: u16, bps: u16) -> Result<lerc_core::DataType> {
    use lerc_core::DataType as D;
    Ok(match (format, bps) {
        (3, 32) => D::F32,
        (3, 64) => D::F64,
        (2, 8) => D::I8,
        (2, 16) => D::I16,
        (2, 32) => D::I32,
        (1 | 4, 8) => D::U8,
        (1 | 4, 16) => D::U16,
        (1 | 4, 32) => D::U32,
        (f, b) => return Err(err(format!("{b}-bit samples of format {f} aren't supported"))),
    })
}

/// A chunk as its blobs must be: `width` pixels wide and `height` rows (at least, in a `strip`),
/// of `samples` a pixel; with `alpha_last`, a last sample that may be an alpha kept as the mask.
struct Shape {
    width: usize,
    height: usize,
    strip: bool,
    samples: usize,
    alpha_last: bool,
}

/// Decodes a chunk's blob: one blob holding every sample of a pixel, or one blob per sample (or a
/// few) after another, each of type `expected`. The chunk's size is bounded, and each header is
/// checked against the chunk, before any samples are decoded. With `alpha_last`, a single blob one
/// sample short holds every sample but an alpha, which its mask gives.
fn decode_blob(blob: &[u8], shape: Shape, expected: lerc_core::DataType) -> Result<Blobs> {
    let Shape { width, height, strip, samples, alpha_last } = shape;
    let pixels = width.checked_mul(height).ok_or_else(|| err("a chunk is too large"))?;
    if pixels.saturating_mul(samples).saturating_mul(8) > MAX_CHUNK_BYTES {
        return Err(err("a chunk is too large"));
    }
    // Each part: its samples a pixel, its values (blob rows cut to `height`) and its mask, whole
    // (a later blob inheriting it needs every row of it).
    let mut parts: Vec<(usize, Vec<f64>, Option<Vec<u8>>)> = Vec::new();
    let mut rest = blob;
    let mut depth = 0usize;
    let mut decoded_bytes = 0usize;
    while depth < samples && !rest.is_empty() {
        // A later band's mask may be left out when it is the one before it: the decoder then
        // gives that one back (and no mask for a band with every pixel valid).
        let inherited = parts.last().and_then(|(_, _, m)| m.clone());
        let info = match &inherited {
            Some(m) => lerc_reader::inspect_first_with_mask(rest, m),
            None => lerc_reader::inspect_first(rest),
        }
        .map_err(err)?;
        let (bw, bh, n) = (info.width as usize, info.height as usize, info.depth.max(1) as usize);
        if bw != width || !(bh == height || strip && bh > height) {
            return Err(err(format!("a blob is {bw} × {bh}, its chunk {width} × {height}")));
        }
        if info.data_type != expected {
            return Err(err(format!("a blob of {:?} samples in an image of {expected:?}", info.data_type)));
        }
        decoded_bytes = decoded_bytes.saturating_add(bw.saturating_mul(bh).saturating_mul(n).saturating_mul(8));
        if decoded_bytes > MAX_CHUNK_BYTES || depth + n > samples {
            return Err(err("a chunk's blobs hold more samples than it does"));
        }
        let d = match &inherited {
            Some(m) => lerc_reader::decode_first_to_f64_with_mask(rest, m),
            None => lerc_reader::decode_first_to_f64(rest),
        }
        .map_err(err)?;
        if d.pixels.len() != bw * bh * n {
            return Err(err("a blob holds fewer samples than it says"));
        }
        // Rows past the strip's own are padding.
        let mut values = d.pixels;
        values.truncate(pixels * n);
        depth += n;
        rest = rest.get(info.blob_size.max(1)..).unwrap_or_default();
        parts.push((n, values, d.mask));
    }
    // GDAL's alpha in the mask: one blob, one sample short.
    if alpha_last && parts.len() == 1 && depth + 1 == samples {
        let (n, v, mask) = parts.into_iter().next().ok_or_else(|| err("a chunk holds no blob"))?;
        let mut values = Vec::with_capacity(pixels * samples);
        for i in 0..pixels {
            values.extend((0..n).map(|k| v.get(i * n + k).copied().unwrap_or(0.0)));
            let opaque = mask.as_ref().is_none_or(|m| m.get(i).is_some_and(|b| *b != 0));
            values.push(if opaque { 255.0 } else { 0.0 });
        }
        return Ok(Blobs { values, valid: None });
    }
    if depth != samples {
        return Err(err(format!("a chunk holds {depth} samples a pixel, its image {samples}")));
    }
    // Interleaved pixel by pixel; each sample valid by its own blob's mask.
    let any_mask = parts.iter().any(|(_, _, m)| m.is_some());
    let mut valid = any_mask.then(|| Vec::with_capacity(pixels * samples));
    if let [(n, _, m)] = parts.as_slice() {
        if let (Some(valid), Some(m)) = (valid.as_mut(), m.as_ref()) {
            for i in 0..pixels {
                valid.extend(std::iter::repeat_n(m.get(i).is_some_and(|b| *b != 0), *n));
            }
        }
        let (_, values, _) = parts.into_iter().next().ok_or_else(|| err("a chunk holds no blob"))?;
        return Ok(Blobs { values, valid });
    }
    let mut values = Vec::with_capacity(pixels * samples);
    for i in 0..pixels {
        for (n, v, m) in &parts {
            values.extend((0..*n).map(|k| v.get(i * n + k).copied().unwrap_or(f64::NAN)));
            if let Some(valid) = valid.as_mut() {
                let ok = m.as_ref().is_none_or(|m| m.get(i).is_some_and(|b| *b != 0));
                valid.extend(std::iter::repeat_n(ok, *n));
            }
        }
    }
    Ok(Blobs { values, valid })
}

/// No samples yet, of the type `format` and `bps` name, with room for `n`.
fn empty(format: u16, bps: u16, n: usize) -> Result<DecodingResult> {
    Ok(match (format, bps) {
        (3, 32) => DecodingResult::F32(Vec::with_capacity(n)),
        (3, 64) => DecodingResult::F64(Vec::with_capacity(n)),
        (2, 8) => DecodingResult::I8(Vec::with_capacity(n)),
        (2, 16) => DecodingResult::I16(Vec::with_capacity(n)),
        (2, 32) => DecodingResult::I32(Vec::with_capacity(n)),
        (1 | 4, 8) => DecodingResult::U8(Vec::with_capacity(n)),
        (1 | 4, 16) => DecodingResult::U16(Vec::with_capacity(n)),
        (1 | 4, 32) => DecodingResult::U32(Vec::with_capacity(n)),
        (f, b) => return Err(err(format!("{b}-bit samples of format {f} aren't supported"))),
    })
}

/// Adds one sample (`fill` for an invalid or missing one) to `out`, in its type. The values come
/// from a blob of the image's own type, so the casts keep them.
fn push(out: &mut DecodingResult, v: Option<f64>, fill: f64) {
    let x = v.unwrap_or(fill);
    match out {
        DecodingResult::F32(o) => o.push(x as f32),
        DecodingResult::F64(o) => o.push(x),
        DecodingResult::I8(o) => o.push(x as i8),
        DecodingResult::I16(o) => o.push(x as i16),
        DecodingResult::I32(o) => o.push(x as i32),
        DecodingResult::U8(o) => o.push(x as u8),
        DecodingResult::U16(o) => o.push(x as u16),
        DecodingResult::U32(o) => o.push(x as u32),
        // Never made by `empty`.
        DecodingResult::F16(_) | DecodingResult::I64(_) | DecodingResult::U64(_) => {}
    }
}

#[cfg(test)]
mod tests {
    //! The files in `tests/data/lerc` were written by GDAL 3.9 (`make_lerc.py` there); their
    //! values follow the formulas computed again here.

    use std::io::Cursor;

    use super::super::{Raster, geotiff};

    fn open(name: &str) -> Raster {
        let path = format!("{}/tests/data/lerc/{name}", env!("CARGO_MANIFEST_DIR"));
        geotiff::read(Cursor::new(std::fs::read(&path).unwrap()), name).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    #[test]
    fn floats_in_tiles_with_a_hole() {
        let r = open("lerc_f32_tiles.tif");
        assert_eq!((r.width, r.height, r.bands.len()), (100, 70, 1));
        for y in 0..70i64 {
            for x in 0..100i64 {
                let v = r.value(0, x, y);
                if (5..9).contains(&y) && (10..20).contains(&x) {
                    assert_eq!(v, None, "({x}, {y}) is in LERC's mask");
                } else {
                    assert_eq!(v, Some(((x * 7 + y * 13) % 1000) as f32 / 8.0), "({x}, {y}), edge tiles included");
                }
            }
        }
    }

    #[test]
    fn three_bands_in_deflate() {
        let r = open("lerc_deflate_u16_3bands.tif");
        assert_eq!((r.width, r.height, r.bands.len()), (70, 50, 3));
        for b in 0..3 {
            for y in 0..50i64 {
                for x in 0..70i64 {
                    assert_eq!(r.value(b, x, y), Some(((x * 3 + y * 5 + b as i64 * 100) % 60000) as f32), "band {b} at ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn planes_in_strips_in_zstandard() {
        let r = open("lerc_zstd_i16_planar_strips.tif");
        assert_eq!((r.width, r.height, r.bands.len()), (45, 30, 2));
        for b in 0..2 {
            for y in 0..30i64 {
                for x in 0..45i64 {
                    assert_eq!(r.value(b, x, y), Some((x - y * 2 + b as i64 * 50) as f32), "band {b} at ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn doubles_in_zstandard() {
        let r = open("lerc_zstd_f64.tif");
        assert_eq!((r.width, r.height), (40, 30));
        for y in 0..30i64 {
            for x in 0..40i64 {
                assert_eq!(r.value(0, x, y), Some((x as f64 * 0.25 - y as f64 * 1.5) as f32), "({x}, {y})");
            }
        }
    }

    #[test]
    fn a_damaged_chunk_is_an_error() {
        let path = format!("{}/tests/data/lerc/lerc_zstd_f64.tif", env!("CARGO_MANIFEST_DIR"));
        let good = std::fs::read(path).unwrap();
        // Every byte past the header flipped in turn (in steps, to stay quick): no panic, and a
        // file that still reads keeps its size.
        for at in (16..good.len()).step_by(7) {
            let mut bad = good.clone();
            if let Some(b) = bad.get_mut(at) {
                *b ^= 0x5a;
            }
            if let Ok(r) = geotiff::read(Cursor::new(bad), "bad") {
                assert_eq!((r.width, r.height), (40, 30));
            }
        }
        // A file cut short in its tiles.
        assert!(geotiff::read(Cursor::new(good.get(..good.len() - 200).unwrap().to_vec()), "cut").is_err());
    }

    #[test]
    fn an_alpha_kept_as_the_mask_is_put_back() {
        // GDAL stores the 0/255 alpha of this RGBA image as LERC's mask: the blob has three samples.
        // A transparent pixel's colour isn't stored: it reads 0, as GDAL reads it.
        let r = open("lerc_u8_rgba_alpha_mask.tif");
        assert_eq!((r.width, r.height, r.bands.len()), (40, 30, 4));
        for y in 0..30i64 {
            for x in 0..40i64 {
                let px = if (x + y) % 3 == 0 { [0, 0, 0, 0] } else { [(x * 5 + y) % 256, (x + y * 7) % 256, (x * 3) % 256, 255] };
                for (b, v) in px.iter().enumerate() {
                    assert_eq!(r.value(b, x, y), Some(*v as f32), "band {b} at ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn each_band_keeps_its_own_mask() {
        // NaN in every band where (x + y) % 5 == 0, and in the first band also where x == 7: GDAL
        // writes a blob a band, each with its mask (the last two the same, the third left to
        // inherit the second's).
        let r = open("lerc_f32_band_masks.tif");
        assert_eq!((r.width, r.height, r.bands.len()), (40, 30, 3));
        for b in 0..3usize {
            for y in 0..30i64 {
                for x in 0..40i64 {
                    let hole = (x + y) % 5 == 0 || (b == 0 && x == 7);
                    let want = (!hole).then_some((x as f64 * 1.5 + y as f64 + b as f64 * 100.0) as f32);
                    assert_eq!(r.value(b, x, y), want, "band {b} at ({x}, {y})");
                }
            }
        }
    }

    /// [`super::decode_blob`] of UInt8 samples (the type the tests' blobs have unless said).
    fn dec(blob: &[u8], width: usize, height: usize, strip: bool, samples: usize, alpha_last: bool) -> crate::Result<super::Blobs> {
        let t = lerc_reader::inspect_first(blob).map(|i| i.data_type).unwrap_or(lerc_core::DataType::U8);
        super::decode_blob(blob, super::Shape { width, height, strip, samples, alpha_last }, t)
    }

    /// A LERC blob of `data` (`depth` samples a pixel), with `mask` (a byte a pixel) if given.
    fn blob<T: lerc_core::Sample>(w: u32, h: u32, depth: u32, data: &[T], mask: Option<&[u8]>) -> Vec<u8> {
        let raster = lerc_core::RasterView::new(w, h, depth, data).unwrap();
        let mask = mask.map(|m| lerc_core::MaskView::new(w, h, m).unwrap());
        lerc_writer::encode(raster, mask, lerc_writer::EncodeOptions::default()).unwrap()
    }

    #[test]
    fn integer_samples_a_mask_drops_are_gaps_not_values() {
        // UInt16, 4 × 3, two samples a pixel; pixels 1 and 6 invalid. GDAL never writes such a
        // mask for integers, other producers can: those samples must become no-data, not 0.
        let data: Vec<u16> = (0..24).map(|v| v as u16 * 10 + 1).collect();
        let mut mask = vec![1u8; 12];
        mask[1] = 0;
        mask[6] = 0;
        let b = dec(&blob(4, 3, 2, &data, Some(&mask)), 4, 3, false, 2, false).unwrap();
        let valid = b.valid.expect("a mask");
        for (i, ok) in valid.iter().enumerate() {
            assert_eq!(*ok, i / 2 != 1 && i / 2 != 6, "sample {i}");
        }
        let mut values: Vec<f32> = b.values.iter().map(|v| *v as f32).collect();
        let gaps: Vec<bool> = valid.iter().map(|v| !v).collect();
        super::blank(&mut values, Some(&gaps));
        assert!(values[2].is_nan() && values[3].is_nan() && values[12].is_nan() && values[13].is_nan());
        assert_eq!(values[0], 1.0);
        // A clean blob says nothing is missing.
        let clean = dec(&blob(4, 3, 2, &data, None), 4, 3, false, 2, false).unwrap();
        assert!(clean.valid.is_none());
    }

    #[test]
    fn a_later_band_inherits_the_mask_before_it() {
        // Two UInt8 bands, one blob each: the writer stores the mask with the first and leaves the
        // second to inherit it.
        let data: Vec<u8> = (0..24).map(|v| v as u8).collect();
        let set = lerc_core::BandSetView::new(4, 3, 1, 2, lerc_core::BandLayout::Bsq, &data).unwrap();
        let mut mask = vec![1u8; 12];
        mask[5] = 0;
        let bytes = lerc_writer::encode_band_set(set, Some(lerc_core::MaskView::new(4, 3, &mask).unwrap()), Default::default()).unwrap();
        let b = dec(&bytes, 4, 3, false, 2, false).unwrap();
        // Pixel by pixel: band 0 then band 1.
        assert_eq!(b.values.get(..4), Some(&[0.0, 12.0, 1.0, 13.0][..]));
        let valid = b.valid.expect("a mask");
        assert_eq!(valid.get(10..12), Some(&[false, false][..]), "pixel 5 in both bands");
        assert_eq!(valid.iter().filter(|v| !**v).count(), 2);
    }

    #[test]
    fn a_padded_last_strip_is_cut_and_a_wrong_size_refused() {
        // A strip coded 5 rows tall for the 3 rows the image has left.
        let data: Vec<u8> = (0..20).map(|v| v as u8).collect();
        let padded = blob(4, 5, 1, &data, None);
        let b = dec(&padded, 4, 3, true, 1, false).unwrap();
        assert_eq!(b.values, (0..12).map(f64::from).collect::<Vec<_>>());
        // A tile must be its full size, and a blob of another width never fits.
        assert!(dec(&padded, 4, 3, false, 1, false).is_err());
        assert!(dec(&padded, 5, 5, true, 1, false).is_err());
        // More samples a pixel than the image has.
        let deep = blob(2, 2, 3, &[0u8; 12], None);
        assert!(dec(&deep, 2, 2, false, 2, false).is_err());
    }

    #[test]
    fn a_blob_of_another_type_is_refused() {
        // Float32 samples where the image says UInt8: an error, not values cast in silence.
        let floats = blob(2, 2, 1, &[0.5f32, 1.5, 2.5, 300.0], None);
        let shape = super::Shape { width: 2, height: 2, strip: false, samples: 1, alpha_last: false };
        let e = super::decode_blob(&floats, shape, lerc_core::DataType::U8).err().expect("refused");
        assert!(e.to_string().contains("F32"), "{e}");
    }

    #[test]
    fn a_band_with_every_pixel_valid_keeps_them_after_a_masked_one() {
        // Two blobs: the first with pixel 4 invalid, the second with every pixel valid (no mask).
        let data: Vec<u8> = (0..12).collect();
        let mut mask = vec![1u8; 12];
        mask[4] = 0;
        let mut both = blob(4, 3, 1, &data, Some(&mask));
        both.extend(blob(4, 3, 1, &data, None));
        let valid = dec(&both, 4, 3, false, 2, false).unwrap().valid.expect("a mask");
        assert_eq!(valid.get(8..10), Some(&[false, true][..]), "pixel 4: band 0 out, band 1 in");
        assert_eq!(valid.iter().filter(|v| !**v).count(), 1);
    }

    #[test]
    fn a_padded_strip_passes_its_whole_mask_on() {
        // Two band blobs 5 rows tall for a last strip of 3, the second inheriting the first's mask:
        // it needs all 20 entries, not the 12 of the image's rows.
        let data: Vec<u8> = (0..40).map(|v| v as u8).collect();
        let set = lerc_core::BandSetView::new(4, 5, 1, 2, lerc_core::BandLayout::Bsq, &data).unwrap();
        let mut mask = vec![1u8; 20];
        mask[2] = 0;
        mask[17] = 0;
        let bytes = lerc_writer::encode_band_set(set, Some(lerc_core::MaskView::new(4, 5, &mask).unwrap()), Default::default()).unwrap();
        let b = dec(&bytes, 4, 3, true, 2, false).unwrap();
        assert_eq!(b.values.len(), 24);
        let valid = b.valid.expect("a mask");
        assert_eq!(valid.get(4..6), Some(&[false, false][..]), "pixel 2 in both bands");
        assert_eq!(valid.iter().filter(|v| !**v).count(), 2, "pixel 17 is padding");
    }

    #[test]
    fn a_chunk_too_large_is_refused_before_decoding() {
        let shape = super::Shape { width: 4096, height: 4096, strip: false, samples: 255, alpha_last: false };
        assert!(super::decode_blob(&[], shape, lerc_core::DataType::U8).is_err());
    }

    #[test]
    fn an_empty_chunk_too_large_is_refused_before_allocating() {
        // The Float64 file's grid, 3 × 2 tiles, made 16384-pixel tiles (2 GiB a tile as decoded),
        // the first tile's byte count 0: refused, never allocated.
        let path = format!("{}/tests/data/lerc/lerc_zstd_f64.tif", env!("CARGO_MANIFEST_DIR"));
        let mut b = std::fs::read(path).unwrap();
        let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as usize;
        let u32_at = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
        let ifd = u32_at(&b, 4);
        let entry = |b: &[u8], tag: usize| (0..u16_at(b, ifd)).map(|k| ifd + 2 + 12 * k).find(|e| u16_at(b, *e) == tag).unwrap();
        let set = |b: &mut Vec<u8>, tag: usize, v: u32| {
            let e = entry(b, tag);
            if u16_at(b, e + 2) == 3 {
                b[e + 8..e + 10].copy_from_slice(&(v as u16).to_le_bytes());
            } else {
                b[e + 8..e + 12].copy_from_slice(&v.to_le_bytes());
            }
        };
        set(&mut b, 256, 3 * 16384);
        set(&mut b, 257, 2 * 16384);
        set(&mut b, 322, 16384);
        set(&mut b, 323, 16384);
        let counts = entry(&b, 325);
        let size = if u16_at(&b, counts + 2) == 3 { 2 } else { 4 };
        let first = u32_at(&b, counts + 8);
        b[first..first + size].fill(0);
        let mut dec = tiff::decoder::Decoder::new(Cursor::new(b)).unwrap().with_limits(tiff::decoder::Limits::unlimited());
        let e = super::chunk(&mut dec, 0, &mut super::Table::default()).err().expect("refused");
        assert!(e.to_string().contains("too large"), "{e}");
    }

    #[test]
    fn a_chunk_with_no_bytes_is_no_data() {
        // The first tile's byte count set to 0, as a sparse file has it.
        let path = format!("{}/tests/data/lerc/lerc_zstd_f64.tif", env!("CARGO_MANIFEST_DIR"));
        let mut b = std::fs::read(path).unwrap();
        let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as usize;
        let u32_at = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
        let ifd = u32_at(&b, 4);
        let counts = (0..u16_at(&b, ifd)).map(|k| ifd + 2 + 12 * k).find(|e| u16_at(&b, *e) == 325).unwrap();
        // GDAL writes these small counts as SHORT (type 3), else LONG.
        let size = if u16_at(&b, counts + 2) == 3 { 2 } else { 4 };
        let first = u32_at(&b, counts + 8);
        b[first..first + size].fill(0);
        let r = geotiff::read(Cursor::new(b), "sparse").unwrap();
        assert_eq!(r.value(0, 0, 0), None);
        assert_eq!(r.value(0, 15, 15), None);
        assert_eq!(r.value(0, 16, 0), Some(4.0), "the next tile reads");
        assert_eq!(r.value(0, 0, 16), Some(-24.0));
    }
}

/// The shared bounded reader must retain a valid Zstandard wrapper's decoded payload.
#[cfg(all(test, not(target_arch = "wasm32")))]
#[test]
fn a_zstandard_wrapper_keeps_its_decoded_bytes() {
    let raw = b"test LERC payload";
    let encoded = zstd::encode_all(raw.as_slice(), 1).unwrap();
    assert_eq!(unwrap_layer(encoded, 2).unwrap(), raw);
}
