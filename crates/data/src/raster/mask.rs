//! GDAL's internal transparency masks: 1-bit images (PhotometricInterpretation 4, NewSubfileType
//! bit 2) in IFDs of their own, one for the full image and one for each overview. The `tiff`
//! crate decodes no such image, so the chunks are decompressed here.

use std::io::{Read, Seek, SeekFrom};

use tiff::decoder::Decoder;
use tiff::tags::Tag;

use super::geotiff::Span;
use crate::{DataError, Result};

/// Most bytes in a chunk, compressed or decoded.
const MAX_CHUNK_BYTES: usize = 256 << 20;
/// Most images looked through for a mask.
const MAX_IMAGES: usize = 4096;

fn err(e: impl std::fmt::Display) -> DataError {
    DataError::Format(e.to_string())
}

/// Where the mask of an image is, as [`find`] looks for it.
pub(super) enum Found {
    /// The mask image's index.
    At(usize),
    /// The file has masks, none of them this image's (an overview without one of its own).
    Elsewhere,
    /// The file has no mask.
    None,
    /// The scan stopped before the end of the file, or an image it passed over can't be read, so
    /// a mask may be there unseen; the reason, in words.
    Unscanned(String),
}

/// The mask of the image at `image` (0: the full one, else an overview) whose size is `size`: the
/// first mask image of that size, an overview's with the reduced-resolution bit set. Leaves the
/// decoder at the mask.
pub(super) fn find<R: Read + Seek>(dec: &mut Decoder<R>, image: usize, size: (u32, u32)) -> Found {
    if let Err(e) = dec.seek_to_image(0) {
        return Found::Unscanned(format!("the image directories can't be read: {e}"));
    }
    let (mut index, mut masks) = (0, false);
    loop {
        let subfile = match dec.find_tag_unsigned::<u32>(Tag::NewSubfileType) {
            Ok(tag) => tag.unwrap_or(0),
            Err(e) => return Found::Unscanned(format!("image {index}'s type can't be read: {e}")),
        };
        if subfile & 4 != 0 {
            let dims = match dec.dimensions() {
                Ok(dims) => dims,
                Err(e) => return Found::Unscanned(format!("image {index}'s size can't be read: {e}")),
            };
            if (subfile & 1 != 0) == (image != 0) && dims == size {
                return Found::At(index);
            }
            masks = true;
        }
        index += 1;
        if !dec.more_images() {
            return if masks { Found::Elsewhere } else { Found::None };
        }
        if index >= MAX_IMAGES {
            return Found::Unscanned(format!("the scan stopped after {MAX_IMAGES} images"));
        }
        if let Err(e) = dec.next_image() {
            return Found::Unscanned(format!("image {index} can't be read: {e}"));
        }
    }
}

/// Which pixels of the mask image at `index` are opaque, for every `step`-th pixel of it across and
/// down (row by row, `ceil(width / step)` × `ceil(height / step)` of them), or of those `span`
/// keeps of them. A mask that can't be read is an error saying why.
pub(super) fn read<R: Read + Seek>(dec: &mut Decoder<R>, index: usize, step: u32, span: Option<Span>) -> Result<Vec<bool>> {
    dec.seek_to_image(index).map_err(err)?;
    let (w, h) = dec.dimensions().map_err(err)?;
    // An absent tag is its default; one that can't be read is an error, not a default.
    let tag = |dec: &mut Decoder<R>, t: Tag| dec.find_tag_unsigned::<u32>(t).map_err(err);
    let bits = dec.find_tag_unsigned_vec::<u16>(Tag::BitsPerSample).map_err(err)?.unwrap_or_else(|| vec![1]);
    if bits != [1] || tag(dec, Tag::SamplesPerPixel)?.is_some_and(|s| s != 1) {
        return Err(err("a transparency mask isn't 1 bit a pixel"));
    }
    let compression = tag(dec, Tag::Compression)?.unwrap_or(1);
    if !matches!(compression, 1 | 5 | 8 | 32773 | 32946) {
        return Err(err(format!("a transparency mask's compression ({compression}) isn't supported")));
    }
    if tag(dec, Tag::Predictor)?.is_some_and(|p| p != 1) {
        return Err(err("a transparency mask with a predictor isn't supported"));
    }
    let lsb_first = tag(dec, Tag::FillOrder)? == Some(2);
    let tiled = tag(dec, Tag::TileWidth)?.is_some();
    let (cw, ch) = if tiled {
        (tag(dec, Tag::TileWidth)?.unwrap_or(0), tag(dec, Tag::TileLength)?.unwrap_or(0))
    } else {
        (w, tag(dec, Tag::RowsPerStrip)?.unwrap_or(h).min(h))
    };
    if cw == 0 || ch == 0 || step == 0 {
        return Err(err("a transparency mask has chunks of no size"));
    }
    let (offsets, counts) = if tiled { (Tag::TileOffsets, Tag::TileByteCounts) } else { (Tag::StripOffsets, Tag::StripByteCounts) };
    let offsets = dec.find_tag_unsigned_vec::<u64>(offsets).map_err(err)?.unwrap_or_default();
    let counts = dec.find_tag_unsigned_vec::<u64>(counts).map_err(err)?.unwrap_or_default();
    // Counted in `u64`: a chunk count, a chunk's corner and the next multiple of `step` pass
    // `u32::MAX` in an image that wide (a panic in a debug build, a wrap in a release build).
    let (across, down) = (u64::from(w.div_ceil(cw)), u64::from(h.div_ceil(ch)));
    let n = usize::try_from(across * down)
        .ok()
        .filter(|n| offsets.len() >= *n && counts.len() >= *n)
        .ok_or_else(|| err("a transparency mask's chunk index is shorter than its image"))?;
    let row_bytes = (cw as usize).div_ceil(8);
    let span = span.unwrap_or_else(|| Span::whole(w.div_ceil(step), h.div_ceil(step)));
    let (ow, oh) = (span.width(), span.height());
    let mut out = super::filled(ow, oh, false)?;
    let step = u64::from(step);
    for c in 0..n {
        crate::progress::check()?;
        let (x0, y0) = ((c as u64 % across) * u64::from(cw), (c as u64 / across) * u64::from(ch));
        let tile =
            Tile { x0, y0, dw: u64::from(cw).min(u64::from(w).saturating_sub(x0)), dh: u64::from(ch).min(u64::from(h).saturating_sub(y0)) };
        // No sampled pixel of the span inside the chunk: skipped.
        if tile.first(step, span).is_none() {
            continue;
        }
        // A tile holds every row, padded; a strip only the image's.
        let rows = if tiled { ch as u64 } else { tile.dh } as usize;
        let need =
            row_bytes.checked_mul(rows).filter(|n| *n <= MAX_CHUNK_BYTES).ok_or_else(|| err("a transparency mask's chunk is too large"))?;
        let (Some(&offset), Some(&count)) = (offsets.get(c), counts.get(c)) else { continue };
        let count =
            usize::try_from(count).ok().filter(|n| *n <= MAX_CHUNK_BYTES).ok_or_else(|| err("a transparency mask's chunk is too large"))?;
        // Read as far as the file goes, not allocated from the claim.
        let r = dec.inner();
        r.seek(SeekFrom::Start(offset)).map_err(err)?;
        let mut encoded = Vec::new();
        r.take(count as u64).read_to_end(&mut encoded).map_err(err)?;
        if encoded.len() < count {
            return Err(err("a transparency mask's chunk is cut short"));
        }
        let decoded = match compression {
            1 => encoded,
            5 => super::lzw::decode(&encoded, need),
            8 | 32946 => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&encoded, need)
                .map_err(|_| err("a transparency mask's chunk doesn't decompress"))?,
            _ => packbits(&encoded, need),
        };
        if (decoded.len() as u64) < (row_bytes as u64).saturating_mul(tile.dh) {
            return Err(err("a transparency mask's chunk is shorter than its image"));
        }
        tile.sample(&mut out, span, step, &decoded, row_bytes, lsb_first);
    }
    Ok(out)
}

/// Where a mask chunk lies in the image: its corner, then the columns and rows of it the image
/// holds, in `u64`.
#[derive(Clone, Copy)]
struct Tile {
    x0: u64,
    y0: u64,
    dw: u64,
    dh: u64,
}

impl Tile {
    /// The sampled columns and rows (half-open) of the chunk that `span` keeps, if it holds any
    /// (every `step`-th column and row from the image's corner).
    fn first(self, step: u64, span: Span) -> Option<((u64, u64), (u64, u64))> {
        let (xs, ys) = (span.kept(span.cols, self.x0, self.dw, step), span.kept(span.rows, self.y0, self.dh, step));
        (xs.0 < xs.1 && ys.0 < ys.1).then_some((xs, ys))
    }

    /// Puts the chunk's sampled pixels that `span` keeps into `out`, the span's width to a row:
    /// `decoded` is its rows of `row_bytes` bytes, a pixel a bit, the first of a byte its high bit
    /// unless `lsb_first`.
    fn sample(self, out: &mut [bool], span: Span, step: u64, decoded: &[u8], row_bytes: usize, lsb_first: bool) {
        let Some(((sx, ex), (sy, ey))) = self.first(step, span) else { return };
        let (ow, sc, sr) = (span.width(), u64::from(span.cols.0), u64::from(span.rows.0));
        let by = usize::try_from(step).unwrap_or(usize::MAX);
        for y in (sy..ey).step_by(by) {
            let row = usize::try_from(y - self.y0)
                .ok()
                .and_then(|r| r.checked_mul(row_bytes))
                .and_then(|at| decoded.get(at..))
                .unwrap_or_default();
            for x in (sx..ex).step_by(by) {
                let (bx, bit) = (usize::try_from((x - self.x0) / 8).unwrap_or(usize::MAX), (x - self.x0) % 8);
                let Some(byte) = row.get(bx) else { continue };
                let on = if lsb_first { byte >> bit & 1 } else { byte >> (7 - bit) & 1 } == 1;
                let at = usize::try_from(y / step - sr)
                    .ok()
                    .and_then(|r| r.checked_mul(ow))
                    .and_then(|at| at.checked_add(usize::try_from(x / step - sc).ok()?));
                if let Some(slot) = at.and_then(|at| out.get_mut(at)) {
                    *slot = on;
                }
            }
        }
    }
}

/// PackBits-decodes `input`, at most `max` bytes of it.
fn packbits(input: &[u8], max: usize) -> Vec<u8> {
    let (mut out, mut i) = (Vec::new(), 0);
    while i < input.len() && out.len() < max {
        let n = input.get(i).map_or(0, |b| *b as i8);
        i += 1;
        if n >= 0 {
            let run = n as usize + 1;
            out.extend_from_slice(input.get(i..(i + run).min(input.len())).unwrap_or_default());
            i += run;
        } else if n != -128 {
            // A repeat with no value byte ends the data: the short chunk is then an error.
            let Some(&byte) = input.get(i) else { break };
            i += 1;
            out.extend(std::iter::repeat_n(byte, (1 - i16::from(n)) as usize));
        }
    }
    out.truncate(max);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A little-endian TIFF of one striped 1-bit mask image, `rows` rows to a strip, its strips
    /// `strips` (already compressed as `compression` says).
    fn striped(w: u32, h: u32, rows: u32, compression: u16, strips: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![b'I', b'I', 42, 0, 0, 0, 0, 0];
        let (mut offsets, mut counts) = (Vec::new(), Vec::new());
        for s in strips {
            offsets.push(out.len() as u32);
            counts.push(s.len() as u32);
            out.extend(s);
        }
        out.resize(out.len() + out.len() % 2, 0);
        let (ifd_at, tags) = (out.len(), 10u32);
        let arrays = ifd_at + 2 + 12 * tags as usize + 4;
        let at = |i: usize| (arrays + 4 * i) as u32;
        let n = strips.len() as u32;
        let entries: [(u16, u16, u32, u32); 10] = [
            (254, 4, 1, 4),
            (256, 4, 1, w),
            (257, 4, 1, h),
            (258, 3, 1, 1),
            (259, 3, 1, u32::from(compression)),
            (262, 3, 1, 4),
            (273, 4, n, if n == 1 { offsets[0] } else { at(0) }),
            (277, 3, 1, 1),
            (278, 4, 1, rows),
            (279, 4, n, if n == 1 { counts[0] } else { at(strips.len()) }),
        ];
        out[4..8].copy_from_slice(&(ifd_at as u32).to_le_bytes());
        out.extend(10u16.to_le_bytes());
        for (tag, ty, count, value) in entries {
            out.extend(tag.to_le_bytes());
            out.extend(ty.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(value.to_le_bytes());
        }
        out.extend([0; 4]);
        out.extend(offsets.iter().chain(&counts).flat_map(|v| v.to_le_bytes()));
        out
    }

    /// The mask of a 10 × 7 image: opaque where `x + y` is even (rows of 2 bytes, the second
    /// holding bits 8 and 9).
    fn rows() -> Vec<Vec<u8>> {
        (0..7)
            .map(|y| {
                let bits = (0..10).fold(0u16, |acc, x| acc | u16::from((x + y) % 2 == 0) << (15 - x));
                bits.to_be_bytes().to_vec()
            })
            .collect()
    }

    fn check(bytes: Vec<u8>) {
        let mut dec = Decoder::new(std::io::Cursor::new(bytes)).expect("decoder");
        assert!(matches!(find(&mut dec, 0, (10, 7)), Found::At(0)));
        let all = read(&mut dec, 0, 1, None).expect("mask");
        assert_eq!(all.len(), 70);
        assert!(all.iter().enumerate().all(|(i, on)| *on == ((i % 10 + i / 10) % 2 == 0)));
        // Every third pixel, from the corner.
        let some = read(&mut dec, 0, 3, None).expect("sampled mask");
        assert_eq!(some.len(), 4 * 3);
        for (i, on) in some.iter().enumerate() {
            assert_eq!(*on, ((i % 4 * 3 + i / 4 * 3) % 2) == 0, "{i}");
        }
        // A part of it, columns 3 to 8 and rows 2 to 6, alone; and of every other pixel.
        for (step, span) in [(1, Span { cols: (3, 8), rows: (2, 6) }), (2, Span { cols: (1, 4), rows: (1, 4) })] {
            let part = read(&mut dec, 0, step, Some(span)).expect("a part of the mask");
            let want: Vec<bool> =
                (span.rows.0..span.rows.1).flat_map(|y| (span.cols.0..span.cols.1).map(move |x| (x * step + y * step) % 2 == 0)).collect();
            assert_eq!(part, want, "every {step}th, {span:?}");
        }
    }

    #[test]
    fn striped_masks_read_raw_deflated_and_packed() {
        let rows = rows();
        // Strips of 3, 3 and 1 rows.
        let groups: Vec<Vec<u8>> = vec![rows[..3].concat(), rows[3..6].concat(), rows[6..].concat()];
        check(striped(10, 7, 3, 1, &groups));
        let zipped: Vec<Vec<u8>> = groups.iter().map(|g| miniz_oxide::deflate::compress_to_vec_zlib(g, 6)).collect();
        check(striped(10, 7, 3, 8, &zipped));
        // PackBits: each strip as one literal run.
        let packed: Vec<Vec<u8>> = groups.iter().map(|g| std::iter::once(g.len() as u8 - 1).chain(g.iter().copied()).collect()).collect();
        check(striped(10, 7, 3, 32773, &packed));
    }

    #[test]
    fn packbits_runs_and_literals() {
        assert_eq!(packbits(&[2, 1, 2, 3, 0xFE, 9, 0x80, 0], 100), [1, 2, 3, 9, 9, 9]);
        assert_eq!(packbits(&[0xFE, 9], 2), [9, 9]);
        // Cut short mid-run: what is there is kept.
        assert_eq!(packbits(&[5, 1, 2], 100), [1, 2]);
        // A run with no value byte: nothing from it.
        assert!(packbits(&[0xF9], 100).is_empty());
    }

    #[test]
    fn a_short_or_missing_chunk_is_an_error_not_a_panic() {
        let rows = rows();
        let mut dec = Decoder::new(std::io::Cursor::new(striped(10, 7, 7, 1, &[rows[..5].concat()]))).expect("decoder");
        assert!(read(&mut dec, 0, 1, None).is_err());
    }

    /// A packbits run that lacks its value byte is a short chunk, not eight transparent pixels.
    #[test]
    fn a_packbits_run_without_its_byte_is_a_short_chunk() {
        let mut dec = Decoder::new(std::io::Cursor::new(striped(8, 8, 8, 32773, &[vec![0xF9]]))).expect("decoder");
        assert!(read(&mut dec, 0, 1, None).is_err());
    }

    /// A damaged image directory chain is said, not taken for a file without a mask.
    #[test]
    fn a_damaged_image_chain_is_unscanned_not_no_mask() {
        let mut bytes = striped(10, 7, 7, 1, &[rows().concat()]);
        // The next image directory points past the end of the file.
        let ifd = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let next = ifd + 2 + 12 * 10;
        bytes[next..next + 4].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
        let mut dec = Decoder::new(std::io::Cursor::new(bytes)).expect("decoder");
        // The overview's mask isn't the first image's, so the scan goes on past it and fails.
        assert!(matches!(find(&mut dec, 1, (10, 7)), Found::Unscanned(_)));
    }

    /// A mask as wide as `u32` counts, in tiles of 2^20 pixels, sampled every 2^20th pixel: the
    /// last tile ends where `u32` does, and the next multiple of the step is past it.
    #[test]
    fn tiles_of_an_image_as_wide_as_u32_holds_are_sampled_and_the_walk_ends() {
        let step = 1u64 << 20;
        let span = Span::whole(4096, 4096);
        let last = Tile { x0: 4095 << 20, y0: 0, dw: (1 << 20) - 1, dh: 1 };
        assert_eq!(last.first(step, span), Some(((4095 << 20, (1 << 32) - 1), (0, 1))));
        for lsb_first in [false, true] {
            let mut row = vec![0u8; 1 << 17];
            row[0] = if lsb_first { 0x01 } else { 0x80 };
            let mut out = vec![false; 4096];
            last.sample(&mut out, Span::whole(4096, 1), step, &row, row.len(), lsb_first);
            assert_eq!(out.iter().filter(|on| **on).count(), 1, "lsb_first {lsb_first}");
            assert!(out[4095], "lsb_first {lsb_first}");
        }
        // Three pixels from 4,294,967,292: the next multiple of 2^20 is 2^32, past them.
        let end = Tile { x0: u64::from(u32::MAX) - 3, y0: 0, dw: 3, dh: 1 };
        assert_eq!(end.first(step, span), None);
        let tall = Tile { x0: 0, y0: u64::from(u32::MAX) - 3, dw: 1, dh: 3 };
        assert_eq!(tall.first(step, span), None);
        let mut out = vec![false; 4];
        end.sample(&mut out, Span::whole(4, 1), step, &[0xff], 1, false);
        assert_eq!(out, [false; 4]);
    }

    /// A mask whose image is too large to hold is an error saying so, not a panic on a vector
    /// of more than `isize::MAX` bytes.
    #[test]
    fn a_mask_too_large_to_hold_is_an_error() {
        let mut dec = Decoder::new(std::io::Cursor::new(striped(u32::MAX, u32::MAX, u32::MAX, 1, &[vec![0]]))).expect("decoder");
        let e = read(&mut dec, 0, 1, None).unwrap_err().to_string();
        assert!(e.contains("not enough memory"), "{e}");
    }
}
