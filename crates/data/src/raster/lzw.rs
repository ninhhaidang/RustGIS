//! LZW-compressed TIFF strips and tiles decoded here, for those the `tiff` crate fails on.
//!
//! Its LZW reader throws away the last bytes of a chunk when the LZW decoder gives them back with
//! the input already used up ("no lzw end code found", issue #350). That depends on the data:
//! about one tile in forty of an elevation model, none in another. GDAL reads such files without a
//! complaint. A chunk the crate fails on is decoded whole here: LZW, then the byte order and the
//! predictor, as the crate would have.

use std::io::{Read, Seek, SeekFrom};

use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::{ByteOrder, Tag};

use crate::{DataError, Result};

/// Most bytes in a chunk, compressed or decoded (the decoder's own intermediate limit).
pub(super) const MAX_CHUNK_BYTES: usize = 256 << 20;

fn err(e: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("GeoTIFF: {e}"))
}

/// Chunk `index` (a strip or tile, counting through every plane) of the decoder's current image,
/// cropped to the image as `Decoder::read_chunk` returns it.
pub(crate) fn chunk<R: Read + Seek>(dec: &mut Decoder<R>, index: u32) -> Result<DecodingResult> {
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
    let bits = dec.find_tag_unsigned_vec::<u16>(Tag::BitsPerSample).ok().flatten().unwrap_or_else(|| vec![1]);
    let bps = *bits.first().unwrap_or(&1);
    if bits.iter().any(|b| *b != bps) || !matches!(bps, 8 | 16 | 32 | 64) {
        return Err(err(format!("{bps}-bit LZW samples aren't supported")));
    }
    let format = dec.find_tag_unsigned_vec::<u16>(Tag::SampleFormat).ok().flatten().and_then(|v| v.first().copied()).unwrap_or(1);
    let spp = tag(dec, Tag::SamplesPerPixel).unwrap_or(1).clamp(1, u32::from(u16::MAX)) as usize;
    let planar = tag(dec, Tag::PlanarConfiguration) == Some(2);
    let samples = if planar { 1 } else { spp };
    let predictor = tag(dec, Tag::Predictor).unwrap_or(1);
    // WhiteIsZero samples are inverted by the crate; left to it.
    if tag(dec, Tag::PhotometricInterpretation) == Some(0) {
        return Err(err("WhiteIsZero LZW images aren't supported"));
    }

    // Where the chunk is in its plane, and how much of it is image.
    let across = w.div_ceil(cw);
    let per_plane = across.saturating_mul(h.div_ceil(ch)).max(1);
    let c = index % per_plane;
    let (x0, y0) = ((c % across.max(1)) * cw, (c / across.max(1)) * ch);
    let dw = if tiled { cw.min(w.saturating_sub(x0)) } else { w } as usize;
    let dh = ch.min(h.saturating_sub(y0)) as usize;
    let size = usize::from(bps / 8);
    let row_bytes = (cw as usize).checked_mul(samples).and_then(|n| n.checked_mul(size)).ok_or_else(|| err("a chunk is too large"))?;
    // A tile holds every row, padded; a strip holds only the image's.
    let rows = if tiled { ch as usize } else { dh };
    let need = row_bytes.checked_mul(rows).filter(|n| *n <= MAX_CHUNK_BYTES).ok_or_else(|| err("a chunk is too large"))?;

    let (offsets, counts) = if tiled { (Tag::TileOffsets, Tag::TileByteCounts) } else { (Tag::StripOffsets, Tag::StripByteCounts) };
    let at = |dec: &mut Decoder<R>, t: Tag| dec.find_tag_unsigned_vec::<u64>(t).ok().flatten().and_then(|v| v.get(index as usize).copied());
    let (Some(offset), Some(count)) = (at(dec, offsets), at(dec, counts)) else {
        return Err(err("no such chunk"));
    };
    let count = usize::try_from(count).ok().filter(|n| *n <= MAX_CHUNK_BYTES).ok_or_else(|| err("a chunk is too large"))?;
    // Read as far as the file goes, not allocated from the claim.
    let r = dec.inner();
    r.seek(SeekFrom::Start(offset)).map_err(err)?;
    let mut encoded = Vec::new();
    r.take(count as u64).read_to_end(&mut encoded).map_err(err)?;
    if encoded.len() < count {
        return Err(err("an LZW chunk is cut short"));
    }

    let decoded = decode(&encoded, need);
    // Every image row must be there; a tile's padding rows may be left out.
    if decoded.len() < row_bytes.saturating_mul(dh) {
        return Err(err("an LZW chunk is shorter than its strip or tile"));
    }

    let little = matches!(dec.byte_order(), ByteOrder::LittleEndian);
    // Converted a row at a time: only a row is ever held as words.
    let mut out = empty(format, bps, dw * dh * samples)?;
    for row in decoded.chunks(row_bytes).take(dh) {
        let mut v = match predictor {
            3 if format == 3 => unshuffle(row, samples, size),
            1 | 2 => row.chunks(size).map(|b| word(b, little)).collect(),
            p => return Err(err(format!("predictor {p} isn't supported for these samples"))),
        };
        if predictor == 2 {
            // Each sample is the difference from the one before it in the row, wrapping at the
            // sample's width (on the bits of floating-point samples too, as libtiff does).
            let mask = if size == 8 { u64::MAX } else { (1u64 << (8 * size)) - 1 };
            for i in samples..v.len() {
                let prev = v.get(i - samples).copied().unwrap_or(0);
                if let Some(x) = v.get_mut(i) {
                    *x = x.wrapping_add(prev) & mask;
                }
            }
        }
        v.truncate(dw * samples);
        append(&mut out, v);
    }
    Ok(out)
}

/// LZW-decodes `input`, at most `max` bytes of it. A stream may stop short of its end code, as
/// libtiff allows; it ends where its bytes do. The output grows as bytes come out, not from what
/// the file's header claims.
pub(super) fn decode(input: &[u8], max: usize) -> Vec<u8> {
    let mut lzw = weezl::decode::Configuration::with_tiff_size_switch(weezl::BitOrder::Msb, 8).build();
    let (mut inp, mut out) = (input, Vec::new());
    let mut buf = vec![0u8; max.min(64 << 10)];
    while out.len() < max {
        let room = (max - out.len()).min(buf.len());
        let r = lzw.decode_bytes(inp, buf.get_mut(..room).unwrap_or_default());
        inp = inp.get(r.consumed_in..).unwrap_or_default();
        out.extend_from_slice(buf.get(..r.consumed_out).unwrap_or_default());
        match r.status {
            Ok(weezl::LzwStatus::Done) => break,
            Ok(_) if r.consumed_in == 0 && r.consumed_out == 0 => break,
            Ok(_) => {}
            // What came before a bad code is kept: the caller checks there's enough.
            Err(_) => break,
        }
    }
    out
}

/// One sample's bytes as an unsigned word, in the file's byte order.
fn word(b: &[u8], little: bool) -> u64 {
    let add = |acc: u64, x: &u8| (acc << 8) | u64::from(*x);
    if little { b.iter().rev().fold(0, add) } else { b.iter().fold(0, add) }
}

/// Undoes the floating-point predictor on one row: bytes differenced along the row, stored most
/// significant byte of every sample first (whatever the file's byte order).
fn unshuffle(row: &[u8], samples: usize, size: usize) -> Vec<u64> {
    let mut b = row.to_vec();
    for i in samples..b.len() {
        let prev = b.get(i - samples).copied().unwrap_or(0);
        if let Some(x) = b.get_mut(i) {
            *x = x.wrapping_add(prev);
        }
    }
    let n = b.len() / size.max(1);
    (0..n).map(|i| (0..size).fold(0u64, |acc, k| (acc << 8) | u64::from(b.get(k * n + i).copied().unwrap_or(0)))).collect()
}

/// No samples yet, of the type `format` and `bps` name, with room for `n`.
fn empty(format: u16, bps: u16, n: usize) -> Result<DecodingResult> {
    Ok(match (format, bps) {
        (3, 32) => DecodingResult::F32(Vec::with_capacity(n)),
        (3, 64) => DecodingResult::F64(Vec::with_capacity(n)),
        (2, 8) => DecodingResult::I8(Vec::with_capacity(n)),
        (2, 16) => DecodingResult::I16(Vec::with_capacity(n)),
        (2, 32) => DecodingResult::I32(Vec::with_capacity(n)),
        (2, 64) => DecodingResult::I64(Vec::with_capacity(n)),
        (1 | 4, 8) => DecodingResult::U8(Vec::with_capacity(n)),
        (1 | 4, 16) => DecodingResult::U16(Vec::with_capacity(n)),
        (1 | 4, 32) => DecodingResult::U32(Vec::with_capacity(n)),
        (1 | 4, 64) => DecodingResult::U64(Vec::with_capacity(n)),
        (f, b) => return Err(err(format!("{b}-bit samples of format {f} aren't supported"))),
    })
}

/// Adds a row's words to `out` as samples of its type.
fn append(out: &mut DecodingResult, words: Vec<u64>) {
    let w = words.into_iter();
    match out {
        DecodingResult::F32(o) => o.extend(w.map(|x| f32::from_bits(x as u32))),
        DecodingResult::F64(o) => o.extend(w.map(f64::from_bits)),
        DecodingResult::I8(o) => o.extend(w.map(|x| x as u8 as i8)),
        DecodingResult::I16(o) => o.extend(w.map(|x| x as u16 as i16)),
        DecodingResult::I32(o) => o.extend(w.map(|x| x as u32 as i32)),
        DecodingResult::I64(o) => o.extend(w.map(|x| x as i64)),
        DecodingResult::U8(o) => o.extend(w.map(|x| x as u8)),
        DecodingResult::U16(o) => o.extend(w.map(|x| x as u16)),
        DecodingResult::U32(o) => o.extend(w.map(|x| x as u32)),
        DecodingResult::U64(o) => o.extend(w),
        // Never made by `empty`.
        DecodingResult::F16(_) => {}
    }
}
