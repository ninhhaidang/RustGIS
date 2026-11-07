//! ASCII grids (`.asc`): a short header, then rows of numbers.

use super::{ColorHint, Crs, Exact, GeoTransform, Raster, Uncounted, max_samples};
use crate::{DataError, Result};

/// Reads an ASCII grid. Without a known coordinate system, coordinates that fit in degrees are
/// taken as WGS 84 longitude/latitude.
pub fn read(text: &str, name: &str, crs: Option<Crs>) -> Result<Raster> {
    read_with_nodata(text, name, crs, None)
}

/// [`read`], with the no-data value a sidecar gives (`.aux.xml`): when the grid has none of its
/// own, its pixels aren't counted as rounded.
pub(super) fn read_with_nodata(text: &str, name: &str, crs: Option<Crs>, sidecar_nodata: Option<f64>) -> Result<Raster> {
    let bad = |m: &str| DataError::Format(format!("ASCII grid: {m}"));
    let mut tokens = text.split_ascii_whitespace().peekable();
    let (mut ncols, mut nrows, mut x, mut y, mut cell, mut dx, mut dy, mut nodata) = (None, None, None, None, None, None, None, None);
    let (mut centre_x, mut centre_y) = (false, false);
    let mut entries = 0;
    // The no-data value exactly as the header writes it (a whole number past 2^53 stays whole).
    let mut nodata_exact = None;
    while let Some(key) = tokens.peek().map(|k| k.to_ascii_lowercase()) {
        if key.parse::<f64>().is_ok() {
            break;
        }
        // A real header has about six entries; give up quickly on text that isn't a grid.
        entries += 1;
        if entries > 16 {
            return Err(bad("not an ASCII grid header"));
        }
        tokens.next();
        let raw = tokens.next();
        let value: f64 = raw.and_then(|v| v.parse().ok()).ok_or_else(|| bad(&format!("bad value for {key}")))?;
        match key.as_str() {
            "ncols" => ncols = Some(value),
            "nrows" => nrows = Some(value),
            "xllcorner" => x = Some(value),
            "yllcorner" => y = Some(value),
            "xllcenter" => {
                x = Some(value);
                centre_x = true;
            }
            "yllcenter" => {
                y = Some(value);
                centre_y = true;
            }
            "cellsize" => cell = Some(value),
            "dx" => dx = Some(value),
            "dy" => dy = Some(value),
            "nodata_value" => {
                nodata = Some(value);
                nodata_exact = raw.and_then(Exact::parse);
            }
            _ => {}
        }
    }
    let size = |v: Option<f64>, what: &str| -> Result<u32> {
        let v = v.ok_or_else(|| bad(&format!("missing {what}")))?;
        if v >= 1.0 && v <= u32::MAX as f64 && v.fract() == 0.0 { Ok(v as u32) } else { Err(bad(&format!("bad {what}"))) }
    };
    let (w, h) = (size(ncols, "ncols")?, size(nrows, "nrows")?);
    let n = (w as usize).checked_mul(h as usize).filter(|n| *n <= max_samples()).ok_or_else(|| bad("too large"))?;
    // CELLSIZE takes precedence over DX/DY, as in GDAL's ASCII grid reader.
    let (dx, dy) = if let Some(cell) = cell { (Some(cell), Some(cell)) } else { (dx, dy) };
    let dx = dx.filter(|c| c.is_finite() && *c > 0.0).ok_or_else(|| bad("missing or invalid cell width"))?;
    let dy = dy.filter(|c| c.is_finite() && *c > 0.0).ok_or_else(|| bad("missing or invalid cell height"))?;
    let (mut x, mut y) = (x.ok_or_else(|| bad("missing xllcorner"))?, y.ok_or_else(|| bad("missing yllcorner"))?);
    if centre_x {
        x -= dx / 2.0;
    }
    if centre_y {
        y -= dy / 2.0;
    }
    // Each value takes at least two characters, so the text bounds what to reserve.
    let mut data = super::room(n.min(text.len() / 2 + 1), w as usize, h as usize)?;
    let mut rounded = 0u64;
    let uncounted = Uncounted::new(nodata_exact, sidecar_nodata);
    for (i, t) in tokens.take(n).enumerate() {
        let v =
            t.parse::<f32>().map_err(|_| bad(&format!("value {} ('{}') isn't a number", i + 1, t.chars().take(20).collect::<String>())))?;
        rounded += u64::from(!held(t, v, uncounted));
        data.push(v);
    }
    if data.len() != n {
        return Err(bad("fewer values than ncols × nrows"));
    }
    let top = y + dy * h as f64;
    let crs = match crs {
        Some(c) => c,
        None if x >= -180.0 && x + dx * w as f64 <= 180.0 && y >= -90.0 && top <= 90.0 => Crs::LonLat,
        None => return Err(bad("unknown coordinate system: put a .prj file next to it")),
    };
    let gt = GeoTransform { origin_x: x, origin_y: top, pixel_width: dx, pixel_height: -dy };
    let mut r = Raster::new(name, w, h, vec![data], gt, crs, nodata, ColorHint::Gray, "Float32")?;
    r.lost.extend(super::planetary::note_of(crs));
    if rounded > 0 {
        r.rounded = vec![rounded];
    }
    Ok(r)
}

/// The powers of ten `f64` holds exactly, 10^0 to 10^22.
const TENS: [f64; 23] =
    [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18, 1e19, 1e20, 1e21, 1e22];

/// Whether `v`, `text` read as `f32`, still gives back `text` at the precision it is written: at most
/// half a unit of its last digit away (a tie writes back the same text: `1234567.8` is held as
/// 1234567.75), so `123.4567` is held (its `f32` is 123.45670319) and
/// `1234567.891` (1234567.875) and `16777217` (16777216) aren't, nor a nonzero value read as zero
/// or infinity (`1e39`, `1e400`, `1e-400`). A subnormal is held at the precision it keeps
/// (`1e-40` is; `1.234567e-40` isn't). NaN, written infinities
/// and no-data values (by [`Uncounted`]'s rule) are held. One pass over the text: up to 6 significant digits
/// are always held in `f32`'s normal range, and up to 15 are compared as a whole number, so only
/// longer or more extreme values are read again as `f64`.
fn held(text: &str, v: f32, nodata: Uncounted) -> bool {
    if v.is_nan() {
        return true;
    }
    // The token is compared exactly as written: a whole number as one, past 2^53 too.
    if let Some(held) = nodata.decides(v, || Exact::parse(text)) {
        return held;
    }
    // Short enough for 6 digits at most (a point and a sign aren't digits), and neither zero nor
    // past f32's range, so nothing underflowed or overflowed: held without looking further.
    let marks = usize::from(text.contains('.')) + usize::from(text.starts_with(['-', '+']));
    if text.len() <= 6 + marks && v.is_finite() && v.abs() >= f32::MIN_POSITIVE {
        return true;
    }
    // The digits from the first nonzero one, as a whole number; how many there are, how many
    // follow the point, and the exponent.
    let (mut whole, mut written, mut decimals, mut point, mut exponent) = (0u64, 0u32, 0i32, false, 0i32);
    for (i, c) in text.bytes().enumerate() {
        match c {
            b'0'..=b'9' => {
                if written > 0 || c != b'0' {
                    written = written.saturating_add(1);
                    whole = whole.wrapping_mul(10).wrapping_add(u64::from(c - b'0'));
                }
                if point {
                    decimals = decimals.saturating_add(1);
                }
            }
            b'.' => point = true,
            b'e' | b'E' => {
                exponent = text.get(i + 1..).and_then(|e| e.parse().ok()).unwrap_or(0);
                break;
            }
            _ => {}
        }
    }
    // A number with a nonzero digit read as zero or infinity underflowed or overflowed.
    if written > 0 && (v == 0.0 || v.is_infinite()) {
        return false;
    }
    if !v.is_finite() || (written <= 6 && (v == 0.0 || v.abs() >= f32::MIN_POSITIVE)) {
        return true;
    }
    // `text` is `whole` × 10^-shift: held when `v` × 10^shift is within half of `whole`.
    let shift = decimals.saturating_sub(exponent);
    if let (true, Some(&ten)) = (written <= 15, TENS.get(shift.unsigned_abs() as usize)) {
        let at = if shift >= 0 { f64::from(v) * ten } else { f64::from(v) / ten };
        return (at.abs() - whole as f64).abs() <= 0.5;
    }
    // A longer whole number is compared as one (an f64 rounds 2^53 + 1); a longer decimal in f64.
    // (An f32 at 2^127 or past can't be an i128, which `as` would saturate to i128::MAX.)
    if let Ok(i) = text.parse::<i128>() {
        return f64::from(v).abs() < 2f64.powi(127) && v as i128 == i;
    }
    let Ok(wide) = text.parse::<f64>() else { return true };
    (f64::from(v) - wide).abs() * 2.0 <= 10f64.powi(shift.saturating_neg())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rectangular_pixels_and_rejects_incomplete_sizes() {
        let corner = "ncols 1\nnrows 2\nxllcorner 10\nyllcorner 20\n{}\n1\n2\n";
        for spacing in ["dx 2\ndy 3", "dy 3\ndx 2"] {
            let r = read(&corner.replace("{}", spacing), "g", None).unwrap();
            assert_eq!((r.transform.origin_x, r.transform.origin_y), (10.0, 26.0));
            assert_eq!((r.transform.pixel_width, r.transform.pixel_height), (2.0, -3.0));
            assert_eq!(r.value(0, 0, 0), Some(1.0));
            assert_eq!(r.value(0, 0, 1), Some(2.0));
            assert_eq!(r.cell_size(), (2.0, 3.0));
            assert_eq!(r.pixel_at(rustgis_geom::Coord::new(11.0, 24.0)), Some((0, 0)));
            let upload = super::super::read_bytes("g.asc", corner.replace("{}", spacing).as_bytes()).unwrap();
            assert_eq!(upload.cell_size(), (2.0, 3.0));
        }
        let center = "ncols 1\nnrows 2\nxllcenter 10\nyllcenter 20\ndx 2\ndy 3\n1\n2\n";
        let r = read(center, "g", None).unwrap();
        assert_eq!((r.transform.origin_x, r.transform.origin_y), (9.0, 24.5));
        assert_eq!((r.transform.pixel_width, r.transform.pixel_height), (2.0, -3.0));
        let mixed = center.replace("yllcenter", "yllcorner");
        let r = read(&mixed, "g", None).unwrap();
        assert_eq!((r.transform.origin_x, r.transform.origin_y), (9.0, 26.0));
        // When both forms are present, CELLSIZE takes precedence, as in GDAL.
        for spacing in ["cellsize 4\ndx 2\ndy 3", "dy 3\ndx 2\ncellsize 4"] {
            let r = read(&corner.replace("{}", spacing), "g", None).unwrap();
            assert_eq!((r.transform.pixel_width, r.transform.pixel_height, r.transform.origin_y), (4.0, -4.0, 28.0));
        }
        for spacing in ["dx 2", "dy 3", "dx 0\ndy 3", "dx 2\ndy -3", "dx NaN\ndy 3", "dx 2\ndy inf", "cellsize 0", "cellsize NaN"] {
            assert!(read(&corner.replace("{}", spacing), "g", None).is_err(), "{spacing}");
        }
        // An unprojected grid extending beyond longitude 180 cannot be inferred as WGS 84.
        let beyond = "ncols 2\nnrows 1\nxllcorner 177\nyllcorner 0\ndx 2\ndy 1\n1 2\n";
        assert!(read(beyond, "g", None).is_err());
        assert!(read(beyond, "g", Some(Crs::WebMercator)).is_ok());
        let at_edge = beyond.replace("177", "176");
        assert!(read(&at_edge, "g", None).is_ok());
        let north = "ncols 1\nnrows 1\nxllcorner 0\nyllcorner 89\ndx 1\ndy 2\n1\n";
        assert!(read(north, "g", None).is_err());
        assert!(read(&north.replace("89", "88"), "g", None).is_ok());
    }

    #[test]
    fn reads_grid() {
        let text = "ncols 3\nnrows 2\nxllcorner 10\nyllcorner 40\ncellsize 1\nNODATA_value -9999\n1 2 3\n4 -9999 6\n";
        let r = read(text, "g", None).unwrap();
        assert_eq!((r.width, r.height), (3, 2));
        assert_eq!(r.value(0, 0, 0), Some(1.0));
        assert_eq!(r.value(0, 1, 1), None);
        assert_eq!(r.transform.origin_y, 42.0);
        assert!(read("ncols 3\nnrows 2\n", "g", None).is_err());
        assert!(read("ncols 1e12\nnrows 1e12\nxllcorner 0\nyllcorner 0\ncellsize 1\n", "g", None).is_err());
        assert!(read("ncols 1\nnrows 1\nxllcorner 5e5\nyllcorner 5e6\ncellsize 10\n1", "g", None).is_err());
        assert!(read("ncols 2\nnrows 1\nxllcorner 0\nyllcorner 0\ncellsize 1\n1 x", "g", None).is_err(), "non-numbers");
        // Centre and corner coordinates per axis.
        let r = read("ncols 1\nnrows 1\nxllcenter 10.5\nyllcorner 40\ncellsize 1\n7", "g", None).unwrap();
        assert_eq!((r.transform.origin_x, r.transform.origin_y), (10.0, 41.0));
    }

    #[test]
    fn values_f32_cannot_hold_are_counted() {
        let grid =
            |values: &str| format!("ncols 4\nnrows 2\nxllcorner 10\nyllcorner 40\ncellsize 1\nNODATA_value -9999.123456789\n{values}");
        // Up to 7 digits that f32 writes back the same, a whole number at 2^24, the no-data value
        // and long zero runs are held.
        let clean = read(&grid("123.4567 16777216 -9999.123456789 12.3\n0.000100 1.5e-3 -0 1000000000"), "g", None).unwrap();
        assert!(clean.rounded.is_empty(), "{:?}", clean.rounded);
        // Past 2^24, too many digits, past f32's range and in its subnormals, they change; a
        // subnormal written to one digit is held.
        let lossy = read(&grid("16777217 1234567.891 1e39 1.234567e-40\n0.123456789 1e-40 1e400 -1e-400"), "g", None).unwrap();
        assert_eq!(lossy.rounded, vec![7]);
        // Long whole numbers are compared exactly: 2^53 + 1 is counted, 2^53 isn't; past 2^53 only
        // the no-data value itself is left out.
        let long = "ncols 2\nnrows 2\nxllcorner 10\nyllcorner 40\ncellsize 1\nNODATA_value -9223372036854775808\n";
        let ids = read(&format!("{long}9007199254740993 9007199254740992 -9223372036854775808 -9223372036854775807"), "g", None).unwrap();
        assert_eq!(ids.rounded, vec![2]);
        // A tie is held: 1234567.8 is f32 1234567.75, which writes back as 1234567.8.
        assert!(read(&grid("1234567.8 1 2 3\n4 5 6 7"), "g", None).unwrap().rounded.is_empty());
        // Written infinities and zeros, and extra zeros, are held.
        let edges = read(&grid("inf -inf 0.000 1.500000000000000000\n0e5 -0.0 1.50000000 2"), "g", None).unwrap();
        assert!(edges.rounded.is_empty(), "{:?}", edges.rounded);
    }

    #[test]
    fn no_data_values_are_left_out_of_the_count_by_one_rule() {
        for c in super::super::NODATA_CASES {
            let header = c.file.map_or(String::new(), |f| format!("NODATA_value {f}\n"));
            let values: Vec<String> = c.samples.iter().map(|v| format!("{v:?}")).collect();
            let text = format!("ncols 2\nnrows 2\nxllcorner 10\nyllcorner 40\ncellsize 1\n{header}{}", values.join(" "));
            let r = read_with_nodata(&text, "c", None, c.sidecar).unwrap();
            assert_eq!(r.rounded.iter().sum::<u64>(), c.counted, "file {:?}, sidecar {:?}", c.file, c.sidecar);
        }
    }

    #[test]
    fn whole_no_data_values_are_compared_exactly() {
        for (file, samples, counted, _) in super::super::NODATA_WHOLE_CASES {
            let values: Vec<String> = samples.iter().map(i64::to_string).collect();
            let text = format!("ncols 2\nnrows 2\nxllcorner 10\nyllcorner 40\ncellsize 1\nNODATA_value {file}\n{}", values.join(" "));
            assert_eq!(read(&text, "c", None).unwrap().rounded.iter().sum::<u64>(), *counted, "no-data {file}");
        }
        // i128::MAX itself reads as f32 2^127, which an i128 can't hold: it is counted.
        let max =
            read("ncols 1\nnrows 1\nxllcorner 10\nyllcorner 40\ncellsize 1\n170141183460469231731687303715884105727", "c", None).unwrap();
        assert_eq!(max.rounded, vec![1]);
    }
}
