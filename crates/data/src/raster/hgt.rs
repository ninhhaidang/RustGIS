//! SRTM height tiles (`.hgt`), read natively on the desktop and in the browser (#2076).
//!
//! The format, from NASA/USGS's SRTM documentation (the SRTM Topography readme
//! `SRTM_Topo.pdf`, and the user guide of the NASA SRTM Version 3.0 products, SRTMGL1/SRTMGL3,
//! at LP DAAC):
//!
//! - A file holds one 1° × 1° tile, named for its south-west corner: `N37W122.hgt` is the tile
//!   from 37°N to 38°N and from 122°W to 121°W (`S` and `E` likewise; latitude in two digits,
//!   longitude in three).
//! - It is a square grid of 16-bit signed, big-endian (Motorola order) heights in meters, with no
//!   header: 1201 × 1201 samples at 3 arc-seconds, or 3601 × 3601 at 1 arc-second. GDAL's SRTMHGT
//!   driver documentation adds 0.5 arc-second tiles (since GDAL 3.10), 7201 × 7201, read here too,
//!   on the same grid rule. Its size alone says which, and nothing else is read: another size is
//!   refused rather than guessed. A 7201 × 7201 tile holds more samples than the browser does, so
//!   there it is read at a step ([`super::step_within`]).
//! - Rows run north to south, and samples within a row west to east.
//! - −32768 is a void (no data).
//! - Horizontal datum WGS 84; heights are above the EGM96 geoid (mean sea level), not the
//!   ellipsoid.
//! - Samples are points on the grid lines, the first and last row and column lying exactly on
//!   the tile's whole-degree edges, so that 1° holds 1201 (3601, 7201) of them and neighboring tiles
//!   share an edge row and column.
//!
//! As a raster, each sample is a cell centered on its point: the raster's cells reach half a cell
//! beyond the whole-degree lines on every side, so a tile's extent is one degree plus one cell
//! wide ([`super::post_transform`]).

use super::{ColorHint, Crs, Raster, max_samples, post_transform, step_within, vertical_note, void_note};
use crate::{DataError, Result};

/// The no-data value of a tile: a void.
pub const VOID: i16 = i16::MIN;
/// The sides a tile can have: 3 arc-seconds (SRTM3), 1 arc-second (SRTM1) and 0.5 arc-second.
pub const SIDES: [usize; 3] = [1201, 3601, 7201];

/// Whether `len` bytes are a tile of one of [`SIDES`], and its side.
pub fn side_of(len: u64) -> Option<usize> {
    SIDES.into_iter().find(|s| (s * s * 2) as u64 == len)
}

/// The south-west corner (latitude, longitude, in whole degrees) of the tile a file name gives:
/// `N37W122.hgt` and `n37w122.hgt` are (37, −122); `S01E006.SRTMGL1.hgt` (the name NASA's
/// downloads have) is (−1, 6). `None` for a name that isn't a tile's, or one that lies outside
/// −90…90° and −180…180°.
pub fn tile_corner(file_name: &str) -> Option<(i32, i32)> {
    let base = std::path::Path::new(file_name).file_name()?.to_str()?;
    let tile = base.get(..7)?;
    // The tile code ends the name or a `.` follows it (`None` when byte 7 is inside a character).
    let rest = base.get(7..)?;
    if !(rest.is_empty() || rest.starts_with('.')) {
        return None;
    }
    let bytes = tile.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<i32> {
        let s = tile.get(r)?;
        if s.bytes().all(|b| b.is_ascii_digit()) { s.parse().ok() } else { None }
    };
    let lat = digits(1..3)?;
    let lon = digits(4..7)?;
    let lat = match bytes.first()?.to_ascii_uppercase() {
        b'N' => lat,
        b'S' => -lat,
        _ => return None,
    };
    let lon = match bytes.get(3)?.to_ascii_uppercase() {
        b'E' => lon,
        b'W' => -lon,
        _ => return None,
    };
    // The tile spans a degree north and east of its corner.
    ((-90..=89).contains(&lat) && (-180..=179).contains(&lon)).then_some((lat, lon))
}

/// Reads an SRTM tile: `bytes` are the file, `file_name` its name (which places it).
///
/// One step that doesn't look at Cancel: the tile is read in a single pass, a row at a time
/// ([`read_checked_from`] streams a file the same way). On the desktop it runs in Add Data's
/// read-ahead, off the UI thread, and a Cancel takes effect when it returns. It stays on one
/// thread: turning big-endian words into numbers is about 0.12 s for the largest (7201 × 7201)
/// tile in a release build, less than reading its 104 MB from disk, so threads would add little.
///
/// Memory: the grid it gives, 4 bytes a height kept (`f32`), which at full resolution is twice the
/// file (a 7201 × 7201 tile: 104 MB on disk, 207 MB as the grid), and at a step no more than
/// [`max_samples`] heights; beside it one row of the file (14 KB at most). Read from `bytes` (an
/// upload, the browser), the file is held too, about 3× the file in all; [`read_checked_from`],
/// which the desktop reads a path with, holds about 2×.
pub fn read(file_name: &str, bytes: &[u8]) -> Result<Raster> {
    read_limited(file_name, bytes, max_samples())
}

fn bad(m: String) -> DataError {
    DataError::Format(format!("SRTM height tile: {m}"))
}

/// Where a tile named `file_name` of `len` bytes lies and its side: its south-west corner
/// (latitude, longitude) and 1201, 3601 or 7201. Checked before a file is read, so a wrong one is
/// refused unread, saying why: a name that isn't a tile's, or a size that isn't one.
pub fn check(file_name: &str, len: u64) -> Result<(i32, i32, usize)> {
    let (lat, lon) = tile_corner(file_name).ok_or_else(|| {
        bad(format!(
            "the file name {file_name:?} doesn't say where the tile is: it must start with its south-west corner, as N37W122.hgt does"
        ))
    })?;
    let side = side_of(len).ok_or_else(|| {
        bad(format!(
            "{len} bytes isn't a tile's size: a tile is 1201 × 1201 heights (2,884,802 bytes), 3601 × 3601 (25,934,402 bytes) or 7201 × 7201 (103,708,802 bytes) ({})",
            nearest_sizes(len)
        ))
    })?;
    Ok((lat, lon, side))
}

/// How `len` bytes, no tile's size, compares with the tiles' sizes nearest it: smaller than the
/// smallest, the file may be cut short; else the tile it is larger than, and the one it is smaller
/// than, if any.
fn nearest_sizes(len: u64) -> String {
    let bytes = |s: usize| (s * s * 2) as u64;
    let below = SIDES.into_iter().rev().find(|s| bytes(*s) < len);
    let above = SIDES.into_iter().find(|s| bytes(*s) > len);
    match (below, above) {
        (None, Some(s)) => format!("{} bytes fewer than a {s} × {s} tile; the file may be cut short", bytes(s) - len),
        (Some(b), Some(a)) => {
            format!("{} bytes more than a {b} × {b} tile, {} fewer than a {a} × {a} one", len - bytes(b), bytes(a) - len)
        }
        (Some(b), None) => format!("{} bytes more than a {b} × {b} tile", len - bytes(b)),
        (None, None) => "no tile is near it".into(),
    }
}

/// [`read`] of a tile [`check`] has placed: `checked` is what it gave for the file's name and
/// size. The bytes are still held to that size (the file may have changed since), but the name
/// isn't parsed again.
pub fn read_checked(file_name: &str, checked: (i32, i32, usize), bytes: &[u8]) -> Result<Raster> {
    read_checked_limited(file_name, checked, bytes, max_samples())
}

/// [`read_checked`] from a reader, a row at a time, so the file's bytes are never held whole:
/// only the heights kept. It must give exactly the tile's size, as `bytes` must. Its peak is the
/// grid and one row: about twice the file at full resolution (see [`read`]).
pub fn read_checked_from(file_name: &str, checked: (i32, i32, usize), source: impl std::io::Read) -> Result<Raster> {
    read_checked_limited(file_name, checked, source, max_samples())
}

/// [`read`], holding at most `max` samples (the tests' small limit exercises the reduction).
fn read_limited(file_name: &str, bytes: &[u8], max: usize) -> Result<Raster> {
    let checked = check(file_name, bytes.len() as u64)?;
    read_checked_limited(file_name, checked, bytes, max)
}

/// [`read_checked_from`], holding at most `max` samples.
fn read_checked_limited(
    file_name: &str,
    (lat, lon, side): (i32, i32, usize),
    mut source: impl std::io::Read,
    max: usize,
) -> Result<Raster> {
    use std::io::Read;
    let wrong_size = |len: u64| bad(format!("the file is {len} bytes now, not the {side} × {side} tile's it was"));
    let step = step_within(side, side, max).ok_or_else(|| bad("the tile is too large to open".into()))?;
    let out = side.div_ceil(step);
    // The grid is what the read holds (twice the file at step 1); the rest is one row.
    let mut data = super::grid::<f32>(out, out)?;
    // One pass, a row at a time: every sample's void counted, every `step`-th kept.
    let row_bytes = side * 2;
    let mut line = Vec::with_capacity(row_bytes);
    let mut read = 0u64;
    let mut voids = 0u64;
    for row in 0..side {
        line.clear();
        let n = source.by_ref().take(row_bytes as u64).read_to_end(&mut line)?;
        read += n as u64;
        if n < row_bytes {
            return Err(wrong_size(read));
        }
        let keep = row % step == 0;
        for (col, raw) in line.as_chunks::<2>().0.iter().enumerate() {
            voids += u64::from(*raw == VOID.to_be_bytes());
            if keep && col % step == 0 {
                data.push(f32::from(i16::from_be_bytes(*raw)));
            }
        }
    }
    // Bytes past the tile's size: the file grew since it was checked.
    let rest = std::io::copy(&mut source, &mut std::io::sink())?;
    if rest > 0 {
        return Err(wrong_size(read + rest));
    }
    let d = 1.0 / (side - 1) as f64;
    let transform = post_transform(f64::from(lon), f64::from(lat), side, d, d, step);
    let name = std::path::Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("srtm").to_string();
    let (w, h) = (out as u32, out as u32);
    let mut r = Raster::new(name, w, h, vec![data], transform, Crs::LonLat, Some(f64::from(VOID)), ColorHint::Gray, "Int16")?;
    if step > 1 {
        // The file's own size; each cell read stands for exactly `step` samples (see `dted::read`).
        r.overview = Some((side as u32, side as u32));
        r.step = step as u32;
        r.posts = true;
    }
    r.lost.push(vertical_note("the EGM96 geoid (mean sea level)"));
    r.lost.extend(void_note(voids, (side * side) as u64, VOID));
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1201 × 1201 tile whose height at (row, col) is `f(row, col)`.
    fn tile(f: impl Fn(usize, usize) -> i16) -> Vec<u8> {
        let mut b = Vec::with_capacity(1201 * 1201 * 2);
        for row in 0..1201 {
            for col in 0..1201 {
                b.extend_from_slice(&f(row, col).to_be_bytes());
            }
        }
        b
    }

    #[test]
    fn names_give_the_south_west_corner() {
        assert_eq!(tile_corner("N37W122.hgt"), Some((37, -122)));
        assert_eq!(tile_corner("/d/n37w122.HGT"), Some((37, -122)));
        assert_eq!(tile_corner("S01E006.SRTMGL1.hgt"), Some((-1, 6)));
        assert_eq!(tile_corner("S90W180.hgt"), Some((-90, -180)));
        assert_eq!(tile_corner("N00E000.hgt"), Some((0, 0)));
        for bad in [
            "N90E000.hgt",
            "N00E180.hgt",
            "S91E000.hgt",
            "X37W122.hgt",
            "N37Q122.hgt",
            "N3xW122.hgt",
            "N37W1222.hgt",
            "srtm.hgt",
            "N37W12.hgt",
            "",
            "N37W122x.hgt",
            "N+7W122.hgt",
            "ÑÑÑÑ.hgt",
            "N37W122é.hgt",
            "N37W122é",
        ] {
            assert_eq!(tile_corner(bad), None, "{bad}");
        }
    }

    #[test]
    fn reads_a_tile_in_place() {
        let bytes = tile(|row, col| match (row, col) {
            (0, 0) => 100,
            (1200, 0) => -5,
            (0, 1200) => 7,
            (600, 600) => VOID,
            (600, 601) => VOID,
            _ => (row % 100) as i16,
        });
        let d = 1.0 / 1200.0;
        for (name, west, south) in
            [("N37W122.hgt", -122.0, 37.0), ("S12E045.hgt", 45.0, -12.0), ("n00e000.hgt", 0.0, 0.0), ("S01W001.hgt", -1.0, -1.0)]
        {
            let r = read(name, &bytes).unwrap();
            assert_eq!((r.width, r.height, r.crs, r.nodata), (1201, 1201, Crs::LonLat, Some(-32768.0)), "{name}");
            let t = r.transform;
            // The cells reach half a cell past the whole-degree lines.
            assert!((t.origin_x - (west - d / 2.0)).abs() < 1e-12, "{name}: {t:?}");
            assert!((t.origin_y - (south + 1.0 + d / 2.0)).abs() < 1e-12, "{name}: {t:?}");
            assert!((t.pixel_width - d).abs() < 1e-15 && (t.pixel_height + d).abs() < 1e-15);
            // The north-west sample is at the corner's whole degrees, rows run north to south.
            let nw = t.to_world(0.5, 0.5);
            assert!((nw.x - west).abs() < 1e-9 && (nw.y - (south + 1.0)).abs() < 1e-9);
            assert_eq!(r.value(0, 0, 0), Some(100.0));
            assert_eq!(r.value(0, 0, 1200), Some(-5.0), "the south-west sample");
            assert_eq!(r.value(0, 1200, 0), Some(7.0), "the north-east sample");
            assert_eq!(r.value(0, 600, 600), None, "a void is no data");
            assert_eq!(r.overview, None);
            assert_eq!(r.lost.len(), 2, "{:?}", r.lost);
            assert!(r.lost[0].contains("EGM96 geoid"), "{:?}", r.lost);
            assert_eq!(r.lost[1], "2 of its 1442401 heights are voids (-32768), read as no data");
        }
        // Tiles at the poles and the antimeridian open, their edges half a cell past the lines.
        for name in ["N89E179.hgt", "S90W180.hgt"] {
            let r = read(name, &bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!r.extent().is_empty(), "{name}");
        }
        // With no voids, nothing is said of them; the vertical reference still is.
        let r = read("N00E000.hgt", &tile(|_, _| 1)).unwrap();
        assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
        assert!(r.lost[0].contains("EGM96"));
    }

    #[test]
    fn wrong_sizes_and_names_are_refused() {
        let bytes = tile(|_, _| 3);
        let e = read("srtm.hgt", &bytes).unwrap_err().to_string();
        assert!(e.contains("doesn't say where the tile is"), "{e}");
        // Smaller than the smallest tile, the file may be cut short.
        let e = read("N37W122.hgt", &bytes[..bytes.len() - 2]).unwrap_err().to_string();
        assert!(
            e.contains("isn't a tile's size") && e.ends_with("(2 bytes fewer than a 1201 × 1201 tile; the file may be cut short)"),
            "{e}"
        );
        // Between sizes, the nearest are given, not a cut.
        let mut longer = bytes.clone();
        longer.extend_from_slice(&[0, 0]);
        let e = read("N37W122.hgt", &longer).unwrap_err().to_string();
        assert!(e.contains("isn't a tile's size") && e.contains("7201 × 7201 (103,708,802 bytes)"), "{e}");
        assert!(
            e.ends_with("(2 bytes more than a 1201 × 1201 tile, 23049598 fewer than a 3601 × 3601 one)") && !e.contains("cut short"),
            "{e}"
        );
        let e = check("N37W122.hgt", 2 * 3601 * 3601 + 2).unwrap_err().to_string();
        assert!(
            e.ends_with("(2 bytes more than a 3601 × 3601 tile, 77774398 fewer than a 7201 × 7201 one)") && !e.contains("cut short"),
            "{e}"
        );
        let e = check("N37W122.hgt", 2 * 7201 * 7201 - 2).unwrap_err().to_string();
        assert!(e.contains("2 fewer than a 7201 × 7201 one") && !e.contains("cut short"), "{e}");
        let e = check("N37W122.hgt", 2 * 7201 * 7201 + 2).unwrap_err().to_string();
        assert!(e.ends_with("(2 bytes more than a 7201 × 7201 tile)") && !e.contains("cut short"), "{e}");
        let e = check("N37W122.hgt", 0).unwrap_err().to_string();
        assert!(e.ends_with("(2884802 bytes fewer than a 1201 × 1201 tile; the file may be cut short)"), "{e}");
        assert_eq!(check("N37W122.hgt", 2 * 3601 * 3601).unwrap(), (37, -122, 3601));
        // 1200 × 1200 or 1201 × 1200 would be guesses.
        assert!(read("N37W122.hgt", &vec![0; 2 * 1200 * 1200]).is_err());
        assert!(read("N37W122.hgt", &vec![0; 2 * 1201 * 1200]).is_err());
    }

    #[test]
    fn a_tile_past_the_budget_is_read_at_every_other_sample() {
        let bytes = tile(|row, col| (row * 2 + col % 7) as i16);
        let r = read_limited("N10E020.hgt", &bytes, 1201 * 1201 - 1).unwrap();
        assert_eq!((r.width, r.height, r.overview, r.step), (601, 601, Some((1201, 1201)), 2));
        assert_eq!(r.value(0, 3, 5), Some(26.0), "file row 10 × 2 + file column 6 % 7");
        // Cells are two samples wide, each centered on the sample kept: output (3, 5) is file
        // column 6, row 10, at 20° + 6d east and 11° − 10d north.
        let d = 1.0 / 1200.0;
        assert!((r.transform.origin_x - (20.0 - d)).abs() < 1e-12);
        assert!((r.transform.origin_y - (11.0 + d)).abs() < 1e-12);
        assert!((r.transform.pixel_width - 2.0 * d).abs() < 1e-15);
        let kept = r.transform.to_world(3.5, 5.5);
        assert!((kept.x - (20.0 + 6.0 * d)).abs() < 1e-9 && (kept.y - (11.0 - 10.0 * d)).abs() < 1e-9, "{kept:?}");
    }

    #[test]
    fn a_half_arc_second_tile_is_read_within_the_budget() {
        // 7201 × 7201 at 0.5″: 51,854,401 samples, more than the browser holds, read at a step.
        let side = 7201;
        let mut bytes = vec![0u8; 2 * side * side];
        let mut put = |row: usize, col: usize, v: i16| {
            let at = 2 * (row * side + col);
            bytes[at..at + 2].copy_from_slice(&v.to_be_bytes());
        };
        put(0, 0, 11);
        put(8 * 3, 8 * 5, 853);
        put(7200, 7200, -4);
        let r = read_limited("N45E006.hgt", &bytes, 1_000_000).unwrap();
        assert_eq!((r.width, r.height, r.overview, r.step), (901, 901, Some((7201, 7201)), 8));
        // Each cell is centered on its post, so the warp's file pixels start 3.5 before the
        // first post's: cell k's center, k + 0.5, is file position 8k + 0.5, the post read.
        assert_eq!((r.file_pixels(), r.file_origin()), ((8.0, 8.0), (-3.5, -3.5)));
        assert_eq!(r.value(0, 0, 0), Some(11.0));
        assert_eq!(r.value(0, 5, 3), Some(853.0));
        assert_eq!(r.value(0, 900, 900), Some(-4.0), "the south-east sample is kept");
        let d = 1.0 / 7200.0;
        let t = r.transform;
        assert!((t.pixel_width - 8.0 * d).abs() < 1e-15 && (t.pixel_height + 8.0 * d).abs() < 1e-15);
        let kept = t.to_world(5.5, 3.5);
        assert!((kept.x - (6.0 + 40.0 * d)).abs() < 1e-9 && (kept.y - (46.0 - 24.0 * d)).abs() < 1e-9, "{kept:?}");
        let se = t.to_world(900.5, 900.5);
        assert!((se.x - 7.0).abs() < 1e-9 && (se.y - 45.0).abs() < 1e-9, "{se:?}");
        // Within the desktop's least budget it would be read whole.
        assert_eq!(step_within(side, side, crate::raster::SAMPLES_FLOOR), Some(1));
        assert_eq!(check("N45E006.hgt", bytes.len() as u64).unwrap(), (45, 6, 7201));
    }

    #[test]
    fn a_checked_tile_is_read_once_its_bytes_agree() {
        let bytes = tile(|_, _| 9);
        let checked = check("N37W122.hgt", bytes.len() as u64).unwrap();
        let r = read_checked("N37W122.hgt", checked, &bytes).unwrap();
        assert_eq!((r.width, r.value(0, 0, 0)), (1201, Some(9.0)));
        // Bytes that no longer match the size checked are refused.
        let e = read_checked("N37W122.hgt", checked, &bytes[..bytes.len() - 2]).unwrap_err().to_string();
        assert!(e.contains("not the 1201 × 1201 tile's it was"), "{e}");
    }

    /// A reader that gives at most `n` bytes a call, as a slow file or a pipe may.
    struct Trickle<'a>(&'a [u8], usize);

    impl std::io::Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.1.min(buf.len()).min(self.0.len());
            let (now, rest) = self.0.split_at(n);
            buf[..n].copy_from_slice(now);
            self.0 = rest;
            Ok(n)
        }
    }

    /// What a read gave, for comparing two reads bit for bit.
    fn same(a: &Raster, b: &Raster) {
        assert_eq!(
            (a.width, a.height, a.transform, a.overview, a.step, a.posts),
            (b.width, b.height, b.transform, b.overview, b.step, b.posts)
        );
        assert_eq!(a.lost, b.lost);
        let bits = |r: &Raster| r.bands.iter().map(|b| b.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>()).collect::<Vec<_>>();
        assert_eq!(bits(a), bits(b));
    }

    #[test]
    fn a_tile_streamed_in_small_pieces_reads_as_its_bytes_do() {
        let bytes = tile(|row, col| if (row + col) % 97 == 0 { VOID } else { (row * 3 + col % 11) as i16 - 40 });
        let checked = check("N37W122.hgt", bytes.len() as u64).unwrap();
        for max in [max_samples(), 1201 * 1201 - 1] {
            let whole = read_limited("N37W122.hgt", &bytes, max).unwrap();
            for n in [1, 3, 4096] {
                let streamed = read_checked_limited("N37W122.hgt", checked, Trickle(&bytes, n), max).unwrap();
                same(&whole, &streamed);
            }
        }
        // A stream cut short, or longer than the tile, is refused as the same bytes are.
        for wrong in [&bytes[..bytes.len() - 3], &[bytes.as_slice(), &[0; 5]].concat()] {
            let from_bytes = read_checked("N37W122.hgt", checked, wrong).unwrap_err().to_string();
            let streamed = read_checked_from("N37W122.hgt", checked, Trickle(wrong, 7)).unwrap_err().to_string();
            assert_eq!(from_bytes, streamed);
        }
        let e = read_checked_from("N37W122.hgt", checked, Trickle(&bytes[..bytes.len() - 3], 7)).unwrap_err().to_string();
        assert!(e.contains("the file is 2884799 bytes now, not the 1201 × 1201 tile's it was"), "{e}");
    }

    #[test]
    fn junk_is_refused() {
        let bytes = tile(|r, c| (r ^ c) as i16);
        // Prefixes, random bytes of a tile's size, and nothing never panic.
        let mut seed = 0x2076u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..64 {
            let cut = (next() % bytes.len() as u64) as usize;
            assert!(read("N37W122.hgt", &bytes[..cut]).is_err());
        }
        let mut random = vec![0u8; bytes.len()];
        for _ in 0..4 {
            random.iter_mut().for_each(|b| *b = next() as u8);
            let r = read("N37W122.hgt", &random).unwrap();
            assert_eq!((r.width, r.height), (1201, 1201));
        }
        assert!(read("N37W122.hgt", &[]).is_err());
        assert!(read("", &[]).is_err());
    }
}
