//! Rasters: bands of numbers on a georeferenced grid (elevation, imagery, classified land cover).
//!
//! Pixels are held as `f32` per band: whole numbers are exact up to ±16,777,216 and decimals to
//! about 7 significant digits. Readers count the values that change on the way in
//! ([`Raster::rounded`]). Readers never panic on malformed input and refuse rasters
//! larger than [`max_samples`]; cloud-optimised GeoTIFFs fall back to their largest overview that
//! fits.

pub mod ascii;
pub mod cog;
pub mod cogwrite;
pub mod crs;
pub(crate) mod epsg_axes;
mod epsg_gap;
mod esri;
#[cfg(not(target_arch = "wasm32"))]
pub mod gdal;
pub mod geotiff;
mod lzw;
pub mod pam;
pub mod planetary;
pub mod wkt;
pub mod zarr;

use std::path::Path;

pub use crs::{Crs, Transformer};
use rustgis_geom::{Coord, Extent};
use serde::{Deserialize, Serialize};

use crate::{DataError, Result};

/// Room for a band of `w × h` samples of `T`, empty: a band the machine can't hold is an error
/// saying so, instead of ending the program (#1291). (In the browser and on Windows the check is
/// reliable; Linux may promise memory it can't give later.)
pub fn grid<T>(w: usize, h: usize) -> Result<Vec<T>> {
    room(w.checked_mul(h).ok_or_else(|| no_memory(w, h))?, w, h)
}

/// A `w × h` band filled with `fill` ([`grid`]).
pub fn filled<T: Clone>(w: usize, h: usize, fill: T) -> Result<Vec<T>> {
    let mut v = grid(w, h)?;
    v.resize(w * h, fill);
    Ok(v)
}

/// A `w × h` band of `values`, its memory reserved first ([`grid`]). More than `w × h` values are
/// an error, not cut off.
pub fn collected<T>(w: usize, h: usize, values: impl IntoIterator<Item = T>) -> Result<Vec<T>> {
    let mut v = grid(w, h)?;
    let mut values = values.into_iter();
    v.extend(values.by_ref().take(w * h));
    if values.next().is_some() {
        return Err(DataError::Format(format!("more values than a {w} × {h} raster holds")));
    }
    Ok(v)
}

/// Room for `n` samples of a `w × h` band (fewer than all of them, when fewer can come).
pub fn room<T>(n: usize, w: usize, h: usize) -> Result<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| no_memory(w, h))?;
    Ok(v)
}

fn no_memory(w: usize, h: usize) -> DataError {
    DataError::Format(format!("not enough memory for a {w} × {h} raster"))
}

/// Most samples (pixels × bands) held in memory in the browser, whose memory is much tighter: 32
/// million. The parts of a GeoTIFF the web app saves are read with it anywhere, so that the same
/// overview is chosen.
pub const BROWSER_SAMPLES: usize = 32_000_000;
/// The fewest samples [`max_samples`] allows: 120 million (480 MB) on the desktop, and
/// [`BROWSER_SAMPLES`] (its limit) in the browser.
#[cfg(not(target_arch = "wasm32"))]
pub const SAMPLES_FLOOR: usize = 120_000_000;
#[cfg(target_arch = "wasm32")]
pub const SAMPLES_FLOOR: usize = BROWSER_SAMPLES;
/// The most [`max_samples`] allows, however much memory the computer has: a billion (4 GB).
pub const SAMPLES_CEILING: usize = 1_000_000_000;

/// Most samples (pixels × bands) held in memory: on the desktop, as many as a quarter of the
/// computer's memory holds at 8 bytes each (a sample takes 4, and reading briefly needs about
/// twice that), from [`SAMPLES_FLOOR`] up to [`SAMPLES_CEILING`] (#1291 step 2, #1318).
pub fn max_samples() -> usize {
    crate::memory::scaled(crate::memory::physical(), SAMPLES_FLOOR, SAMPLES_CEILING, 8)
}
/// Largest ASCII grid file read (1 GiB).
pub const MAX_ASCII_BYTES: u64 = 1 << 30;
/// File extensions read as rasters.
pub const READ_EXTENSIONS: &[&str] = &["tif", "tiff", "asc", "png", "jpg", "jpeg"];

/// Whether a file name is a raster this module reads.
pub fn is_raster_name(name: &str) -> bool {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    READ_EXTENSIONS.contains(&ext.as_str())
}

/// Whether a file name is JPEG 2000's (`.jp2`, `.j2k`): read through GDAL on the desktop, from a
/// download too (`gdal::convert_jpeg2000`).
pub fn is_jpeg2000_name(name: &str) -> bool {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "jp2" | "j2k")
}

/// The extension of JPEG 2000 contents, from their first bytes: `jp2` for a JP2 file's signature
/// box, `j2k` for a bare codestream's first markers (SOC, SIZ). `None` for anything else. The
/// bytes alone decide, so a download without a file name can be told apart on every platform.
pub fn jpeg2000_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\0\0\0\x0cjP  \r\n\x87\n") {
        Some("jp2")
    } else if bytes.starts_with(&[0xff, 0x4f, 0xff, 0x51]) {
        Some("j2k")
    } else {
        None
    }
}

/// Maps pixel (column, row) corners to coordinates: north-up grids only.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeoTransform {
    /// Top-left corner of the top-left pixel.
    pub origin_x: f64,
    pub origin_y: f64,
    /// Pixel size; `pixel_height` is negative for north-up rasters.
    pub pixel_width: f64,
    pub pixel_height: f64,
}

impl GeoTransform {
    /// Coordinates of a pixel position (corners at whole numbers, centres at +0.5).
    pub fn to_world(&self, col: f64, row: f64) -> Coord {
        Coord::new(self.origin_x + col * self.pixel_width, self.origin_y + row * self.pixel_height)
    }
    /// Pixel position of coordinates.
    pub fn to_pixel(&self, c: Coord) -> (f64, f64) {
        ((c.x - self.origin_x) / self.pixel_width, (c.y - self.origin_y) / self.pixel_height)
    }
    pub fn is_valid(&self) -> bool {
        [self.origin_x, self.origin_y, self.pixel_width, self.pixel_height].iter().all(|v| v.is_finite())
            && self.pixel_width != 0.0
            && self.pixel_height != 0.0
    }
}

/// Bins in [`BandStats::histogram`].
pub const HISTOGRAM_BINS: usize = 256;

/// Summary statistics of one band's valid pixels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BandStats {
    pub count: usize,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub std_dev: f64,
    /// Pixel counts in [`HISTOGRAM_BINS`] equal bins from `min` to `max`.
    pub histogram: Vec<u64>,
    /// The distinct values, when the band holds whole numbers and has at most 256 of them.
    pub categories: Option<Vec<f64>>,
}

impl BandStats {
    /// Statistics of the finite values in `data`.
    pub fn from_values(data: &[f32]) -> BandStats {
        BandStats::compute(data, f32::is_finite)
    }

    fn compute(data: &[f32], valid: impl Fn(f32) -> bool + Sync) -> BandStats {
        BandStats::compute_with(data, valid, None)
    }

    /// [`BandStats::compute`], taking the range, mean and spread from `stored` (statistics of the
    /// whole band, when `data` is a reduced overview of it).
    fn compute_with(data: &[f32], valid: impl Fn(f32) -> bool + Sync, stored: Option<pam::Stats>) -> BandStats {
        BandStats::compute_on(data, valid, stored, threads_for(data.len()))
    }

    /// [`BandStats::compute_with`] on `threads` threads. On one, one pass makes the count, range,
    /// sums and whether every value is whole, and a second the histogram and distinct values. On
    /// several, the sums are made on this thread, in the band's order, so the mean and spread are
    /// the same to the bit; what doesn't depend on the order is counted on the others
    /// ([`on_runs`]), the range while this thread sums. The range keeps the first of equal values
    /// (−0 or 0) in the band's order either way.
    fn compute_on(data: &[f32], valid: impl Fn(f32) -> bool + Sync, stored: Option<pam::Stats>, threads: usize) -> BandStats {
        let ranged = |run: &[f32], sum: bool| {
            let (mut n, mut min, mut max, mut whole, mut sums) = (0usize, f64::INFINITY, f64::NEG_INFINITY, true, (0.0f64, 0.0f64));
            for &v in run.iter().filter(|&&v| valid(v)) {
                let x = v as f64;
                n += 1;
                (min, max) = (lower(min, x), higher(max, x));
                whole &= x.fract() == 0.0;
                if sum {
                    sums = (sums.0 + x, sums.1 + x * x);
                }
            }
            (n, min, max, whole, sums)
        };
        let (n, mut min, mut max, whole, (sum, sum2)) = if threads <= 1 {
            ranged(data, true)
        } else {
            let sums = || ranged(data, true).4;
            let (runs, sums) = on_runs_beside(data, threads, sums, |run| ranged(run, false));
            let mut all = (0usize, f64::INFINITY, f64::NEG_INFINITY, true, sums);
            for (rn, rmin, rmax, rwhole, _) in runs {
                all = (all.0 + rn, lower(all.1, rmin), higher(all.2, rmax), all.3 && rwhole, all.4);
            }
            all
        };
        if n == 0 {
            return BandStats::default();
        }
        let mut mean = sum / n as f64;
        let mut std_dev = (sum2 / n as f64 - mean * mean).max(0.0).sqrt();
        if let Some(s) = stored {
            (min, max) = (min.min(s.min), max.max(s.max));
            mean = s.mean.unwrap_or(mean);
            std_dev = s.std_dev.unwrap_or(std_dev);
        }
        let span = max - min;
        // Whole numbers within 16 bits are tracked in a bit set (fast for byte imagery); others in
        // a small set that gives up past 256 values.
        let small_ints = whole && min >= 0.0 && max <= 65_535.0;
        let k = (HISTOGRAM_BINS as f64 - 1.0) / if span > 0.0 { span } else { 1.0 };
        let b = Binning { min, k, small_ints, whole };
        let mut runs = on_runs(data, threads, |run| Tally::run(run, &valid, b)).into_iter();
        let mut tally = runs.next().unwrap_or_else(|| Tally::run(&[], &valid, b));
        for t in runs {
            tally.histogram.iter_mut().zip(&t.histogram).for_each(|(a, b)| *a += b);
            tally.bits.iter_mut().zip(&t.bits).for_each(|(a, b)| *a |= b);
            tally.track &= t.track;
            if tally.track {
                tally.distinct.extend(t.distinct);
                tally.track = tally.distinct.len() <= 256;
            }
        }
        let Tally { histogram, bits, distinct, track } = tally;
        let categories = if small_ints {
            let n: u32 = bits.iter().map(|w| w.count_ones()).sum();
            (n <= 256).then(|| {
                bits.iter()
                    .enumerate()
                    .flat_map(|(wi, w)| (0..64).filter(move |b| w & (1 << b) != 0).map(move |b| (wi * 64 + b) as f64))
                    .collect()
            })
        } else {
            track.then(|| distinct.into_iter().map(|v| v as f64).collect())
        };
        BandStats { count: n, min, max, mean, std_dev, histogram, categories }
    }

    /// The value below which `p` (0–100) percent of pixels fall, from the histogram.
    pub fn percentile(&self, p: f64) -> f64 {
        let total: u64 = self.histogram.iter().sum();
        if total == 0 || !p.is_finite() {
            return self.min;
        }
        let target = (p.clamp(0.0, 100.0) / 100.0 * total as f64).max(0.0);
        let mut acc = 0u64;
        let step = (self.max - self.min) / (HISTOGRAM_BINS as f64 - 1.0);
        for (i, c) in self.histogram.iter().enumerate() {
            acc += c;
            if acc as f64 >= target {
                return self.min + i as f64 * step;
            }
        }
        self.max
    }
}

/// How [`Tally::run`] counts a band's values: its histogram's bins from `min`, `k` bins to a unit,
/// and how its distinct values are kept (`small_ints`: in a bit set; else, when `whole`, in a
/// set that gives up past 256 values).
#[derive(Clone, Copy)]
struct Binning {
    min: f64,
    k: f64,
    small_ints: bool,
    whole: bool,
}

/// A band's values counted: its histogram, and its distinct values (the bit set of whole
/// numbers within 16 bits, or the set and whether it still keeps them).
struct Tally {
    histogram: Vec<u64>,
    bits: Vec<u64>,
    distinct: std::collections::BTreeSet<i64>,
    track: bool,
}

/// Values below which a band is counted on one thread (#1252): threads pay off past about a
/// million.
const PARALLEL_VALUES: usize = 1 << 20;

/// `x` when it is below `min`, else `min`: equal values (−0 and 0) keep the first.
fn lower(min: f64, x: f64) -> f64 {
    if x < min { x } else { min }
}

/// `x` when it is above `max`, else `max`: equal values (−0 and 0) keep the first.
fn higher(max: f64, x: f64) -> f64 {
    if x > max { x } else { max }
}

/// The threads to count a band of `len` values on: at most [`crate::threads::max`], one per
/// 256 Ki values, and one below [`PARALLEL_VALUES`] or in the browser.
fn threads_for(len: usize) -> usize {
    if cfg!(target_arch = "wasm32") || len < PARALLEL_VALUES { 1 } else { crate::threads::max().min(len / (PARALLEL_VALUES / 4)) }
}

/// `f` over `threads` runs of `data` that cover it in order, its results in that order, the
/// calling thread taking the first. A worker's panic is raised again here; a run whose thread
/// can't start is made on this one.
fn on_runs<T: Send>(data: &[f32], threads: usize, f: impl Fn(&[f32]) -> T + Sync) -> Vec<T> {
    on_runs_beside(data, threads, || (), f).0
}

/// [`on_runs`], the calling thread running `beside` first, while the other threads start on
/// their runs.
fn on_runs_beside<T: Send, U>(data: &[f32], threads: usize, beside: impl FnOnce() -> U, f: impl Fn(&[f32]) -> T + Sync) -> (Vec<T>, U) {
    if threads <= 1 || data.is_empty() {
        let beside = beside();
        return (vec![f(data)], beside);
    }
    let mut runs = data.chunks(data.len().div_ceil(threads));
    let first = runs.next().unwrap_or_default();
    let f = &f;
    std::thread::scope(|scope| {
        let started: Vec<_> = runs.map(|run| std::thread::Builder::new().spawn_scoped(scope, move || f(run)).map_err(|_| run)).collect();
        let beside = beside();
        let mut out = vec![f(first)];
        for run in started {
            out.push(match run {
                Ok(h) => h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)),
                // The OS refused a thread: made here.
                Err(run) => f(run),
            });
        }
        (out, beside)
    })
}

impl Tally {
    /// `data`'s valid values counted on this thread.
    fn run(data: &[f32], valid: &impl Fn(f32) -> bool, b: Binning) -> Tally {
        let mut histogram = vec![0u64; HISTOGRAM_BINS];
        let mut bits = if b.small_ints { vec![0u64; 1024] } else { Vec::new() };
        let mut distinct = std::collections::BTreeSet::new();
        let mut track = b.whole;
        for &v in data {
            if !valid(v) {
                continue;
            }
            let x = v as f64;
            let bin = ((x - b.min) * b.k).round() as usize;
            if let Some(h) = histogram.get_mut(bin.min(HISTOGRAM_BINS - 1)) {
                *h += 1;
            }
            if b.small_ints {
                let i = x as usize;
                if let Some(w) = bits.get_mut(i / 64) {
                    *w |= 1 << (i % 64);
                }
            } else if track {
                distinct.insert(x as i64);
                track = distinct.len() <= 256;
            }
        }
        Tally { histogram, bits, distinct, track }
    }
}

/// The no-data value the readers leave out of [`Raster::rounded`], by one rule for every reader:
///
/// - The value is the file's when it is finite, else the sidecar's (`.aux.xml`) when that is
///   finite, else none: the value [`Raster::new`] and [`Raster::apply_pam`] go on to mask with.
/// - A sample whose value in the file is exactly that value isn't counted, even when its `f32`
///   overflows (a Float64 no-data of −1.8e308).
/// - Any other sample `is_valid` masks is counted, as it went from a value to no-data: one that
///   rounds onto the no-data value (an Int32 ID 16777217 with no-data 16777216), or one `f32`
///   holds exactly that the no-data value rounds onto (−2^63 with no-data −2^63 + 1).
/// - Any other sample is counted when it changed, an overflow to infinity included.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Uncounted {
    exact: Option<Exact>,
    masked: Option<f32>,
}

/// A value exactly as a file states it: a whole number as one (an `f64` rounds those past 2^53),
/// anything else as an `f64`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Exact {
    Whole(i128),
    Float(f64),
}

impl Exact {
    /// A number as text: whole when it is written as one.
    pub(crate) fn parse(text: &str) -> Option<Exact> {
        let text = text.trim();
        text.parse::<i128>().map(Exact::Whole).ok().or_else(|| text.parse::<f64>().ok().map(Exact::Float))
    }

    /// The value as an `f64` (rounded, for a whole number past 2^53).
    pub(crate) fn wide(self) -> f64 {
        match self {
            Exact::Whole(w) => w as f64,
            Exact::Float(x) => x,
        }
    }

    /// Whether two values are the same number.
    pub(crate) fn same(self, other: Exact) -> bool {
        match (self, other) {
            (Exact::Whole(a), Exact::Whole(b)) => a == b,
            (Exact::Float(a), Exact::Float(b)) => a == b,
            // Only an integral float within i128's range (the cast saturates past it) can be one.
            (Exact::Whole(w), Exact::Float(x)) | (Exact::Float(x), Exact::Whole(w)) => {
                x.fract() == 0.0 && x.abs() < 2f64.powi(127) && x as i128 == w
            }
        }
    }
}

impl Uncounted {
    /// The value from the file's own no-data value and the sidecar's.
    pub(crate) fn new(file: Option<Exact>, sidecar: Option<f64>) -> Uncounted {
        let exact = file.filter(|v| v.wide().is_finite()).or(sidecar.filter(|v| v.is_finite()).map(Exact::Float));
        Uncounted { exact, masked: exact.map(|d| d.wide() as f32).filter(|d| d.is_finite()) }
    }

    /// Whether a sample read as `f` is held, when the no-data value decides it: `Some(true)` for
    /// the no-data value itself; `Some(false)` for any other sample [`Raster::is_valid`] masks as
    /// no-data, exact in `f32` or not, as it went from a value to no-data; `None` otherwise (the
    /// reader's own test decides). `value` gives the sample's value in the file; it is asked for
    /// only when `f` matches the value's `f32` (or both are infinite, for a value past `f32`'s
    /// range).
    pub(crate) fn decides(self, f: f32, value: impl FnOnce() -> Option<Exact>) -> Option<bool> {
        let is = |v: Option<Exact>| self.exact.zip(v).is_some_and(|(a, b)| a.same(b));
        match self.masked {
            Some(m) if f == m => Some(is(value())),
            None if self.exact.is_some() && f.is_infinite() && is(value()) => Some(true),
            _ => None,
        }
    }
}

/// Cases of [`Uncounted`]'s rule that every reader's tests read (a no-data value that isn't finite,
/// one past `f32`'s range, a sidecar's, a sample rounding onto it), so the readers agree.
#[cfg(test)]
pub(crate) struct NodataCase {
    /// The file's own no-data value, as it writes it (a GeoTIFF tag, an ASCII grid header, a
    /// Zarr `_FillValue`).
    pub file: Option<&'static str>,
    /// The sidecar's (`.aux.xml`; Zarr stores have none).
    pub sidecar: Option<f64>,
    /// The samples of a 2 × 2 Float64 band.
    pub samples: [f64; 4],
    /// How many a GeoTIFF or ASCII grid counts as rounded.
    pub counted: u64,
    /// How many a Zarr store counts: it has no sidecar.
    pub zarr: u64,
}

#[cfg(test)]
pub(crate) const NODATA_CASES: &[NodataCase] = &[
    // No no-data value: too many digits and an overflow to infinity are both counted.
    NodataCase { file: None, sidecar: None, samples: [0.123_456_789_012_345, 1.0, -1e39, 2.0], counted: 2, zarr: 2 },
    // A NaN or infinite file value is ignored and the sidecar's is used: 0.1 isn't counted, but
    // 0.1000000001, a value that rounds onto it and so becomes no-data, is. Zarr, with no
    // sidecar, counts all three.
    NodataCase { file: Some("nan"), sidecar: Some(0.1), samples: [0.1, 0.100_000_000_1, 0.123_456_789_012_345, 2.0], counted: 2, zarr: 3 },
    NodataCase { file: Some("inf"), sidecar: Some(0.1), samples: [0.1, 0.100_000_000_1, 0.123_456_789_012_345, 2.0], counted: 2, zarr: 3 },
    // Past f32's range: the no-data value itself isn't counted, another overflow is.
    NodataCase { file: Some("-1.7976931348623157e308"), sidecar: None, samples: [f64::MIN, -1e39, 0.5, 2.0], counted: 1, zarr: 1 },
    NodataCase { file: Some("-1e39"), sidecar: None, samples: [-1e39, -2e39, 0.5, 2.0], counted: 1, zarr: 1 },
    // A finite file value wins over the sidecar's; 1.00000001 and the ID 16777217 round onto it
    // and become no-data, so they are counted, while the no-data value itself isn't.
    NodataCase { file: Some("1"), sidecar: Some(0.1), samples: [1.000_000_01, 0.123_456_789_012_345, 0.5, 2.0], counted: 2, zarr: 2 },
    NodataCase { file: Some("16777216"), sidecar: None, samples: [16_777_217.0, 16_777_216.0, 0.5, 2.0], counted: 1, zarr: 1 },
];

/// Cases of [`Uncounted`]'s rule for 64-bit whole numbers past 2^53, which an `f64` rounds: the
/// file's no-data value as it writes it, four Int64 samples, how many a GeoTIFF or ASCII grid
/// counts, and how many a Zarr store (which masks exact matches only) counts.
#[cfg(test)]
pub(crate) const NODATA_WHOLE_CASES: &[(&str, [i64; 4], u64, u64)] = &[
    // −2^63 + 1 is the no-data value, written and stored exactly, so it isn't counted. −2^63, which
    // f32 holds, shares its f32: a GeoTIFF or grid masks it as no-data and counts it; Zarr keeps it.
    ("-9223372036854775807", [i64::MIN + 1, i64::MIN + 1, i64::MIN, 1], 1, 0),
    // −2^63 is the no-data value; −2^63 + 1 rounds onto it and is counted everywhere.
    ("-9223372036854775808", [i64::MIN, i64::MIN + 1, 0, 1], 1, 1),
];

/// One band of pixels, row by row.
#[derive(Clone)]
pub struct Band {
    pub name: String,
    pub data: Vec<f32>,
    pub stats: BandStats,
    /// The names of the band's classes, by value, from GDAL's `.aux.xml` sidecar (see
    /// [`Band::class_name`]). Empty when the band names no class.
    pub classes: Vec<String>,
    /// The band's central wavelength in nanometres, from GDAL's metadata (the file's tag or its
    /// sidecar, [`pam::wavelength_nm`]); `None` when the file gives none.
    pub wavelength: Option<f64>,
}

impl Band {
    /// The name of the class of value `v`: `None` when `v` isn't a whole number or has no name.
    pub fn class_name(&self, v: f64) -> Option<&str> {
        if v < 0.0 || v.fract() != 0.0 {
            return None;
        }
        self.classes.get(v as usize).map(String::as_str).filter(|s| !s.is_empty())
    }
}

/// The longest a raster read in place is waited for natively: its overview as a layer opens, its
/// tiles as it is exported.
pub const IN_PLACE_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// An alpha band's opaque value for a pixel type (`UInt16`: 65535): the type's largest, 1 for
/// floating point (as TIFF keeps it), 255 for bytes and anything else.
pub fn alpha_max(data_type: &str) -> f32 {
    match data_type {
        "Int8" => 127.0,
        "Int16" => 32767.0,
        "UInt16" => 65535.0,
        "Int32" => 2_147_483_647.0,
        "UInt32" => 4_294_967_295.0,
        "Int64" => 9.223_372e18,
        "UInt64" => 1.844_674_4e19,
        "Float16" | "Float32" | "Float64" => 1.0,
        _ => 255.0,
    }
}

/// How a raster's bands were meant to be shown, from the file.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ColorHint {
    #[default]
    Gray,
    /// Band 1 is grey and band 2 its transparency.
    GrayAlpha,
    /// Bands 1–3 are red, green, blue (and 4 is alpha, if present).
    Rgb { alpha: bool },
    /// Band 1 indexes this colour table (RGBA).
    Palette(Vec<[u8; 4]>),
}

/// A raster on a grid: georeferenced, unless `georeferenced` is false (a scan, whose transform
/// is only a stand-in).
#[derive(Clone)]
pub struct Raster {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub bands: Vec<Band>,
    pub transform: GeoTransform,
    pub crs: Crs,
    pub nodata: Option<f64>,
    pub color: ColorHint,
    /// Pixel type in the file (`UInt8`, `Float32`…).
    pub data_type: String,
    /// Where it was read from, if anywhere.
    pub source: Option<String>,
    /// Whether the pixels come from a reduced-resolution overview.
    pub overview: Option<(u32, u32)>,
    /// `false` for a scan (a TIFF with no GeoTIFF tags, a PNG, a JPEG): `transform` is then only
    /// a stand-in until the picture is placed or georeferenced, and says nothing about the ground.
    pub georeferenced: bool,
    /// What reading it left out or cut (a colour table, names), one line a kind with a count,
    /// for whoever adds it to the map to say (AGENTS.md, Never silent). Empty when nothing was
    /// left out.
    pub lost: Vec<String>,
    /// How many values of each band changed when read as `f32` (Int32 IDs past 16,777,216, Float64
    /// values with more digits than `f32` holds), by band; empty when none did. NaN values and
    /// samples that are the no-data value aren't counted (one that rounds onto it is). The two are
    /// compared once read as `f32`: a long decimal whose `f32` reading differs from the no-data
    /// value's (rounded through `f64`) is counted, as it isn't masked as no-data either.
    pub rounded: Vec<u64>,
    /// By band, whether the band holds some values rounded when its source was read, how many
    /// not known: a raster made from part of one with `rounded` values (a clip, a pixel edit)
    /// keeps an unknown share of them, so it carries this mark instead of a count. Empty when
    /// none does.
    pub rounded_some: Vec<bool>,
    /// Who holds the rights to the picture (the TIFF Copyright tag): the credits a map's tile
    /// layers ask for, on a GeoTIFF of the view. Read from a GeoTIFF that has it, and written
    /// when this raster is exported; a raster made from it (clipped, reprojected) starts
    /// without.
    pub copyright: Option<String>,
    transformer: Transformer,
    extent: Extent,
}

impl std::fmt::Debug for Raster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Raster")
            .field("name", &self.name)
            .field("size", &(self.width, self.height, self.bands.len()))
            .field("crs", &self.crs)
            .field("transform", &self.transform)
            .finish_non_exhaustive()
    }
}

/// A band's own name, for a file's metadata: `None` while it keeps its number (`Band 3`).
pub(crate) fn band_name_of(r: &Raster, band: usize) -> Option<&str> {
    r.bands.get(band).map(|b| b.name.as_str()).filter(|n| !n.trim().is_empty() && *n != format!("Band {}", band + 1))
}

/// GDAL's metadata tag (42112) for the bands `bands` of `r`, written as samples in that order:
/// their names and wavelengths, and the first one's class names. Both GeoTIFF writers take it
/// from here.
pub(crate) fn gdal_metadata_of(r: &Raster, bands: &[usize]) -> Option<String> {
    let names: Vec<Option<&str>> = bands.iter().map(|b| band_name_of(r, *b)).collect();
    let wavelengths: Vec<Option<f64>> = bands.iter().map(|b| r.bands.get(*b).and_then(|b| b.wavelength)).collect();
    let first = bands.first().and_then(|b| r.bands.get(*b)).map_or(&[][..], |b| &b.classes[..]);
    pam::gdal_metadata(&names, first, &wavelengths)
}

/// Text for a TIFF ASCII tag, which holds 7-bit ASCII only: control characters (a NUL would end
/// it early) as spaces, `©` and `®` as `(c)` and `(R)`, any other character outside ASCII as `?`
/// ([`tag_text_changes`] counts them), trimmed.
pub fn tag_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '©' => out.push_str("(c)"),
            '®' => out.push_str("(R)"),
            c if c.is_control() => out.push(' '),
            c if c.is_ascii() => out.push(c),
            _ => out.push('?'),
        }
    }
    out.trim().to_string()
}

/// What [`tag_text`] changes in `s` (the trimmed ends not counted): how many `©` and `®` it
/// writes as `(c)` and `(R)`, how many characters as `?`, and how many control characters as
/// spaces, line breaks and tabs among them.
pub fn tag_text_changes(s: &str) -> (usize, usize, usize) {
    let inner = s.trim_matches(|c: char| c.is_control() || c.is_ascii_whitespace());
    let symbols = inner.chars().filter(|c| matches!(c, '©' | '®')).count();
    let questioned = inner.chars().filter(|c| !c.is_ascii() && !c.is_control() && !matches!(c, '©' | '®')).count();
    (symbols, questioned, inner.chars().filter(|c| c.is_control()).count())
}

/// What writing `s` to a TIFF ASCII tag ([`tag_text`]) changes, for an export to say: `None`
/// when it is written as it is (or is empty); that no tag is written when nothing of it is left.
pub fn tag_text_note(s: &str) -> Option<String> {
    // Nothing a tag can hold (control characters only): the writers write no tag.
    if tag_text(s).is_empty() {
        return (!s.is_empty()).then(|| "the credits hold nothing a Copyright tag can, so the file has none".to_string());
    }
    let (symbols, questioned, spaced) = tag_text_changes(s);
    let count = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    let parts: Vec<String> = [
        (symbols, count(symbols, "© or ® as (c) or (R)", "©s or ®s as (c) or (R)")),
        (questioned, count(questioned, "character as ?", "characters as ?")),
        (spaced, count(spaced, "control character as a space", "control characters as spaces")),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(_, part)| part)
    .collect();
    (!parts.is_empty()).then(|| format!("the Copyright tag holds ASCII only, so it is written with {}", parts.join(", ")))
}

/// How many of the names [`gdal_metadata_of`] writes for `bands` hold a character XML can't (a
/// control character), which is written as a space: for a writer to say.
pub fn gdal_metadata_changed(r: &Raster, bands: &[usize]) -> usize {
    let names = bands.iter().filter_map(|b| band_name_of(r, *b));
    let first = bands.first().and_then(|b| r.bands.get(*b)).map_or(&[][..], |b| &b.classes[..]);
    names.chain(first.iter().map(String::as_str)).filter(|n| n.chars().any(pam::xml_cannot_hold)).count()
}

impl Raster {
    /// Builds a raster from bands of `width × height` pixels and computes statistics.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        width: u32,
        height: u32,
        bands: Vec<Vec<f32>>,
        transform: GeoTransform,
        crs: Crs,
        nodata: Option<f64>,
        color: ColorHint,
        data_type: impl Into<String>,
    ) -> Result<Raster> {
        Raster::build(name, width, height, bands, transform, crs, nodata, color, data_type, true)
    }

    /// [`Raster::new`] without statistics (each band's are empty): a part of a larger raster,
    /// drawn with the whole's (a cloud-optimised GeoTIFF's view, read in place).
    #[allow(clippy::too_many_arguments)]
    pub fn unmeasured(
        name: impl Into<String>,
        width: u32,
        height: u32,
        bands: Vec<Vec<f32>>,
        transform: GeoTransform,
        crs: Crs,
        nodata: Option<f64>,
        color: ColorHint,
        data_type: impl Into<String>,
    ) -> Result<Raster> {
        Raster::build(name, width, height, bands, transform, crs, nodata, color, data_type, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        name: impl Into<String>,
        width: u32,
        height: u32,
        bands: Vec<Vec<f32>>,
        transform: GeoTransform,
        crs: Crs,
        nodata: Option<f64>,
        color: ColorHint,
        data_type: impl Into<String>,
        measure: bool,
    ) -> Result<Raster> {
        let n = (width as usize).checked_mul(height as usize).ok_or_else(|| DataError::Format("raster too large".into()))?;
        if n == 0 || bands.is_empty() {
            return Err(DataError::Format("the raster has no pixels".into()));
        }
        if bands.iter().any(|b| b.len() != n) {
            return Err(DataError::Format("band sizes don't match the raster size".into()));
        }
        if !transform.is_valid() {
            return Err(DataError::Format("the raster has no usable georeferencing".into()));
        }
        let transformer = crs.transformer().map_err(DataError::Format)?;
        let nd = nodata.filter(|v| v.is_finite()).map(|v| v as f32);
        let valid = move |v: f32| v.is_finite() && nd.is_none_or(|d| v != d);
        let bands = bands
            .into_iter()
            .enumerate()
            .map(|(i, data)| Band {
                name: format!("Band {}", i + 1),
                stats: if measure { BandStats::compute(&data, valid) } else { BandStats::default() },
                data,
                classes: Vec::new(),
                wavelength: None,
            })
            .collect();
        let mut r = Raster {
            name: name.into(),
            width,
            height,
            bands,
            transform,
            crs,
            nodata: nodata.filter(|v| v.is_finite()),
            color,
            data_type: data_type.into(),
            source: None,
            overview: None,
            georeferenced: true,
            lost: Vec::new(),
            rounded: Vec::new(),
            rounded_some: Vec::new(),
            copyright: None,
            transformer,
            extent: Extent::EMPTY,
        };
        r.extent = r.compute_extent();
        if r.extent.is_empty() {
            return Err(DataError::Format("the raster's location couldn't be worked out".into()));
        }
        Ok(r)
    }

    /// Applies what GDAL's `.aux.xml` sidecar says about the bands: their descriptions name them,
    /// their category names name their classes, its no-data value applies when the file has none
    /// (the file's own is kept, as GDAL keeps it, and one it gives otherwise said), and its statistics stand in for those of a reduced overview (the whole band's, where the
    /// overview's only approximate them). What it left out is said as `from`'s ("its GDAL
    /// metadata", "its .aux.xml sidecar").
    pub fn apply_pam(&mut self, pam: &pam::Pam, from: &str) {
        self.lost.extend(pam.lost.iter().map(|l| format!("{from}: {l}")));
        // Names and wavelengths for bands the raster doesn't have have nowhere to go.
        let beyond = pam
            .bands
            .iter()
            .filter(|b| {
                (b.band == 0 || b.band > self.bands.len())
                    && (b.description.is_some() || !b.categories.is_empty() || b.wavelength.is_some())
            })
            .count();
        if beyond > 0 {
            self.lost
                .push(format!("{from}: names or wavelengths for {beyond} band(s) it doesn't have (it has {}) left out", self.bands.len()));
        }
        // So is a no-data value: it isn't the raster's.
        let outside = pam.bands.iter().filter(|b| (b.band == 0 || b.band > self.bands.len()) && b.nodata.is_some()).count();
        if outside > 0 {
            self.lost.push(format!("{from}: a no-data value for {outside} band(s) it doesn't have (it has {}) left out", self.bands.len()));
        }
        let mut recompute = false;
        if self.nodata.is_none() {
            self.nodata = pam.nodata_within(self.bands.len());
            recompute = self.nodata.is_some();
            // Bands that disagree give none: their no-data pixels are read as values, and said.
            let mut given: Vec<f64> =
                pam.bands.iter().filter(|b| (1..=self.bands.len()).contains(&b.band)).filter_map(|b| b.nodata).collect();
            given.sort_by(f64::total_cmp);
            given.dedup();
            if self.nodata.is_none() && given.len() > 1 {
                let values: Vec<String> = given.iter().take(4).map(|v| v.to_string()).collect();
                let more = if given.len() > 4 { ", …" } else { "" };
                self.lost.push(format!(
                    "{from}: its bands give different no-data values ({}{more}): none is applied, so those pixels are read as values",
                    values.join(", ")
                ));
            }
        } else if let Some(own) = self.nodata {
            // The file's own no-data value is kept; a sidecar's other one for its bands is said.
            let mut other: Vec<f64> = pam
                .bands
                .iter()
                .filter(|b| (1..=self.bands.len()).contains(&b.band))
                .filter_map(|b| b.nodata)
                .filter(|v| !(v == &own || (v.is_nan() && own.is_nan())))
                .collect();
            other.sort_by(f64::total_cmp);
            other.dedup();
            if !other.is_empty() {
                let values: Vec<String> = other.iter().take(4).map(|v| v.to_string()).collect();
                let more = if other.len() > 4 { ", …" } else { "" };
                self.lost
                    .push(format!("{from}: its no-data value ({}{more}) isn't the file's own ({own}), which is kept", values.join(", ")));
            }
        }
        let nd = self.nodata.map(|v| v as f32);
        let valid = move |v: f32| v.is_finite() && nd.is_none_or(|d| v != d);
        let overview = self.overview.is_some();
        for (i, band) in self.bands.iter_mut().enumerate() {
            let p = pam.band(i + 1);
            if let Some(d) = p.and_then(|p| p.description.as_ref()) {
                band.name = d.clone();
            }
            if let Some(p) = p.filter(|p| !p.categories.is_empty()) {
                band.classes = p.categories.clone();
            }
            if let Some(w) = p.and_then(|p| p.wavelength) {
                band.wavelength = Some(w);
            }
            let stored = p.and_then(|p| p.stats).filter(|_| overview);
            if recompute || stored.is_some() {
                band.stats = BandStats::compute_with(&band.data, valid, stored);
            }
        }
    }

    /// Longitude/latitude bounds, from points along the grid's edges.
    fn compute_extent(&self) -> Extent {
        let mut e = Extent::EMPTY;
        let (w, h) = (self.width as f64, self.height as f64);
        let steps = 32;
        let mut add = |col: f64, row: f64| {
            if let Some(c) = self.transformer.to_lonlat_for_extent(self.transform.to_world(col, row)) {
                e.expand(Coord::new(c.x, c.y.clamp(-90.0, 90.0)));
            }
        };
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            for (col, row) in [(t * w, 0.0), (t * w, h), (0.0, t * h), (w, t * h)] {
                add(col, row);
            }
        }
        // Interior points too: in polar and other projections the grid can reach further than its
        // edges do. (A raster crossing the antimeridian still gets a world-wide extent.)
        let n = 16;
        for i in 1..n {
            for j in 1..n {
                add(w * i as f64 / n as f64, h * j as f64 / n as f64);
            }
        }
        // Normalize the transformed range as a whole, retaining its winding: a range crossing
        // the antimeridian needs world-wide bounds to include both strips in a single Extent.
        // A native full turn remains world-wide even if a datum shift contracts its samples.
        if !e.is_empty() && self.transformer.is_geographic() && w * self.transform.pixel_width.abs() >= 360.0 {
            (e.min_x, e.max_x) = (-180.0, 180.0);
        } else if !e.is_empty() && (e.min_x < -180.0 || e.max_x > 180.0) {
            let shift = 360.0 * ((e.min_x + 180.0) / 360.0).floor();
            let (lo, hi) = (e.min_x - shift, e.max_x - shift);
            (e.min_x, e.max_x) = if lo >= -180.0 && hi <= 180.0 { (lo, hi) } else { (-180.0, 180.0) };
        }
        e
    }

    /// Refuses a raster with no coordinates, for the tools that would give their result a place
    /// on the ground (the result of georeferencing it first is another matter).
    pub fn require_coordinates(&self) -> std::result::Result<(), String> {
        if self.georeferenced {
            Ok(())
        } else {
            Err(format!("{} has no coordinates: georeference it first (raster.georeference)", self.name))
        }
    }

    /// This raster as a scan: its pixels with no coordinates, as `"as": "scan"` opens a picture.
    pub fn into_scan(mut self) -> Result<Raster> {
        if self.georeferenced {
            self.crs = Crs::LonLat;
            self.transformer = self.crs.transformer().map_err(DataError::Format)?;
            self.transform = unreferenced(self.width, self.height);
            self.georeferenced = false;
            self.extent = self.compute_extent();
            if self.extent.is_empty() {
                return Err(DataError::Format("the picture's location couldn't be worked out".into()));
            }
        }
        Ok(self)
    }

    /// Stretches a scan over a longitude/latitude box (west, south, east, north), as the
    /// `raster.georeference` extent does. It stays marked as not georeferenced.
    pub fn place(&mut self, bbox: [f64; 4]) -> Result<()> {
        let [w, s, e, n] = bbox;
        if self.georeferenced {
            return Err(DataError::Format("only a raster with no coordinates can be placed".into()));
        }
        if !(bbox.iter().all(|v| v.is_finite()) && w < e && s < n) {
            return Err(DataError::Format("the box is [west, south, east, north]".into()));
        }
        let old = self.transform;
        self.transform = GeoTransform {
            origin_x: w,
            origin_y: n,
            pixel_width: (e - w) / f64::from(self.width),
            pixel_height: -(n - s) / f64::from(self.height),
        };
        let extent = self.compute_extent();
        if !self.transform.is_valid() || extent.is_empty() {
            self.transform = old;
            return Err(DataError::Format("the scan's location couldn't be worked out".into()));
        }
        self.extent = extent;
        Ok(())
    }

    /// Longitude/latitude bounds.
    pub fn extent(&self) -> Extent {
        self.extent
    }

    pub fn transformer(&self) -> &Transformer {
        &self.transformer
    }

    pub fn band_count(&self) -> usize {
        self.bands.len()
    }

    /// Whether band `b` holds values rounded when its source was read: counted in `rounded`, or
    /// marked in `rounded_some` with no count.
    pub fn band_rounded(&self, b: usize) -> bool {
        self.rounded.get(b).is_some_and(|n| *n > 0) || self.rounded_some.get(b).copied().unwrap_or(false)
    }

    /// Marks this raster, made from part of `src` band for band (a clip, a pixel edit, a
    /// resample), as holding some of `src`'s rounded values, how many unknown: the bands of `src`
    /// that held any are marked, and the count, which was `src`'s whole, isn't kept.
    pub fn keep_rounded_mark(&mut self, src: &Raster) {
        self.keep_rounded_marks(std::iter::once(src));
    }

    /// As [`Raster::keep_rounded_mark`], for a raster made from parts of several (a mosaic): a
    /// band is marked when any source's band of that number held rounded values.
    pub fn keep_rounded_marks<'a>(&mut self, srcs: impl IntoIterator<Item = &'a Raster>) {
        let mut marks = vec![false; self.band_count()];
        for src in srcs {
            marks.iter_mut().enumerate().for_each(|(b, m)| *m |= src.band_rounded(b));
        }
        self.rounded = Vec::new();
        self.rounded_some = if marks.contains(&true) { marks } else { Vec::new() };
    }

    /// Whether a value is a real measurement (not no-data or NaN).
    pub fn is_valid(&self, v: f32) -> bool {
        // Compared in f32, as the pixels (and the statistics) are: a no-data value such as
        // -3.4e38 or -9999.1 isn't exactly representable in f32.
        v.is_finite() && self.nodata.is_none_or(|d| v != d as f32)
    }

    /// The value of `band` at pixel (`col`, `row`), unless it is no-data or outside the grid.
    pub fn value(&self, band: usize, col: i64, row: i64) -> Option<f32> {
        if col < 0 || row < 0 || col >= self.width as i64 || row >= self.height as i64 {
            return None;
        }
        let v = *self.bands.get(band)?.data.get(row as usize * self.width as usize + col as usize)?;
        self.is_valid(v).then_some(v)
    }

    /// The pixel under a longitude/latitude point.
    pub fn pixel_at(&self, lonlat: Coord) -> Option<(i64, i64)> {
        let (c, r) = self.transform.to_pixel(self.transformer.from_lonlat(lonlat)?);
        let (c, r) = (self.wrap_col(c).floor(), r.floor());
        (c >= 0.0 && r >= 0.0 && c < self.width as f64 && r < self.height as f64).then_some((c as i64, r as i64))
    }

    /// A column off a geographic grid moved round by whole turns of 360° onto it, when that
    /// lands on it: a grid stored from 0 to 360° has the place at -100° at its 260° column.
    /// Any other column is returned as it is.
    pub fn wrap_col(&self, c: f64) -> f64 {
        let w = f64::from(self.width);
        if (0.0..w).contains(&c) || !self.transformer.is_geographic() {
            return c;
        }
        let turn = 360.0 / self.transform.pixel_width.abs();
        let wrapped = c.rem_euclid(turn);
        if wrapped < w { wrapped } else { c }
    }

    /// Cell size in the raster's units (x, y).
    pub fn cell_size(&self) -> (f64, f64) {
        (self.transform.pixel_width.abs(), self.transform.pixel_height.abs())
    }

    /// Ground size of a pixel in metres (approximate for geographic rasters).
    pub fn cell_size_m(&self) -> (f64, f64) {
        let (x, y) = self.cell_size();
        if self.crs.is_geographic() {
            let lat = self.extent.center().y.to_radians().cos().abs().max(0.01);
            (x * 111_320.0 * lat, y * 110_574.0)
        } else {
            (x, y)
        }
    }
}

/// Reads a raster file by path.
pub fn read_path(path: &Path) -> Result<Raster> {
    // A Zarr store is a folder: its first variable.
    #[cfg(not(target_arch = "wasm32"))]
    if zarr::is_store(path) {
        return zarr::read_dir(path, "", None);
    }
    // Only regular files (not FIFOs or devices), and ASCII grids within a size limit, as they
    // are read whole.
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(DataError::Format(format!("{} is not a regular file", path.display())));
    }
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("raster").to_string();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    // The `.aux.xml` sidecar, read first: a no-data value it gives (for a file that has none) keeps
    // those pixels out of the rounded count as they are read.
    let pam = pam::read(path);
    let sidecar_nodata = pam.as_ref().and_then(pam::Pam::nodata);
    let mut r = match ext.as_str() {
        "tif" | "tiff" => {
            #[cfg(not(target_arch = "wasm32"))]
            let r = geotiff::read_file_with_nodata(path, &name, sidecar_nodata)?;
            #[cfg(target_arch = "wasm32")]
            let r = geotiff::read_with_nodata(std::io::BufReader::new(std::fs::File::open(path)?), &name, sidecar_nodata)?;
            r
        }
        "png" | "jpg" | "jpeg" => read_scan(&name, std::io::BufReader::new(std::fs::File::open(path)?))?,
        "asc" => {
            if meta.len() > MAX_ASCII_BYTES {
                return Err(DataError::Format("the ASCII grid is too large to open".into()));
            }
            let text = std::fs::read_to_string(path)?;
            read_ascii(path, &text, &name, sidecar_nodata)?
        }
        #[cfg(not(target_arch = "wasm32"))]
        _ if gdal::is_gdal_raster_name(&path.to_string_lossy()) => {
            // Converted once to a GeoTIFF in the cache; the layer still names this file.
            return read_converted(path, &gdal::convert(path, None, None)?);
        }
        other => return Err(DataError::Unsupported(format!(".{other}"))),
    };
    // `apply_pam` takes the no-data of the bands the raster has, which the decoder may not have
    // left out of the rounded count: it left out the sidecar's no-data for all bands. When the two
    // differ (a record for a band the raster lacks, or one only some bands give), the file is read
    // again with the one applied (none, when the bands it has give none), so the rounded count
    // leaves out just its pixels. Only an ASCII grid, or a GeoTIFF of a type `f32` can round, is read again.
    let applied = pam.as_ref().map(|p| p.nodata_within(r.bands.len()));
    if let Some(nd) = applied
        && nd != sidecar_nodata
        && r.nodata.is_none()
        && (ext == "asc"
            || (matches!(ext.as_str(), "tif" | "tiff")
                && matches!(r.data_type.as_str(), "UInt32" | "Int32" | "UInt64" | "Int64" | "Float64")))
    {
        // The second read gives all the first did: the first goes before it, so two aren't held.
        drop(r);
        r = if ext == "asc" {
            read_ascii(path, &std::fs::read_to_string(path)?, &name, nd)?
        } else {
            #[cfg(not(target_arch = "wasm32"))]
            let again = geotiff::read_file_with_nodata(path, &name, nd)?;
            #[cfg(target_arch = "wasm32")]
            let again = geotiff::read_with_nodata(std::io::BufReader::new(std::fs::File::open(path)?), &name, nd)?;
            again
        };
    }
    if let Some(p) = &pam {
        r.apply_pam(p, pam::SIDECAR);
    }
    r.source = Some(path.display().to_string());
    Ok(r)
}

/// Reads a raster file, with the GDAL choices a raster GDAL reads may have been added with: its
/// subdataset (a NetCDF variable) and the coordinate system it doesn't say. Other files ignore
/// them.
pub fn read_path_with(path: &Path, subdataset: Option<&str>, crs: Option<&str>) -> Result<Raster> {
    // A Zarr store's variable and slice (`t2m[time=3]`).
    #[cfg(not(target_arch = "wasm32"))]
    if zarr::is_store(path) {
        return zarr::read_dir(path, subdataset.unwrap_or(""), crs);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if (subdataset.is_some() || crs.is_some()) && gdal::is_gdal_raster_name(&path.to_string_lossy()) {
        return read_converted(path, &gdal::convert(path, subdataset, crs)?);
    }
    let _ = (subdataset, crs);
    read_path(path)
}

/// Reads the GeoTIFF GDAL converted `path` to, named and sourced as `path` (and its subdataset).
#[cfg(not(target_arch = "wasm32"))]
pub fn read_converted(path: &Path, c: &gdal::Converted) -> Result<Raster> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("raster");
    let mut r = geotiff::read_file(&c.path, stem)?;
    if let Some(s) = &c.subdataset {
        r.name = format!("{stem} {}", gdal::short_name(s));
    }
    r.source = Some(path.display().to_string());
    Ok(r)
}

/// The stand-in transform of a scan of `w` × `h` pixels: square pixels, the longer side one degree,
/// centred on 0°, 0°. Only [`Raster::place`] or georeferencing gives it a place.
pub(crate) fn unreferenced(w: u32, h: u32) -> GeoTransform {
    let side = f64::from(w.max(h).max(1));
    GeoTransform {
        origin_x: -0.5 * f64::from(w) / side,
        origin_y: 0.5 * f64::from(h) / side,
        pixel_width: 1.0 / side,
        pixel_height: -1.0 / side,
    }
}

/// Every `k`-th pixel of interleaved `ch`-sample pixels, one vector per band.
fn deinterleave<T: Copy + Into<f32>>(raw: &[T], w: u32, h: u32, ch: usize, k: u32) -> Result<Vec<Vec<f32>>> {
    let step = k.max(1) as usize;
    let (ow, oh) = ((w as usize).div_ceil(step), (h as usize).div_ceil(step));
    let mut bands: Vec<Vec<f32>> = (0..ch).map(|_| grid(ow, oh)).collect::<Result<_>>()?;
    for row in (0..h as usize).step_by(step) {
        for col in (0..w as usize).step_by(step) {
            let at = (row * w as usize + col) * ch;
            for (c, band) in bands.iter_mut().enumerate() {
                band.push(raw.get(at + c).map_or(f32::NAN, |v| (*v).into()));
            }
        }
    }
    Ok(bands)
}

/// Reads a PNG or JPEG as a raster with no coordinates (`georeferenced` is false): its bands as
/// they are (grey, grey and alpha, RGB, RGBA; 8 or 16 bits). A picture is decoded whole, so one
/// whose decoded size passes four times [`max_samples`] bytes is refused (save it as a TIFF); one
/// with more than [`max_samples`] samples is read at every second pixel, with its `overview` set.
pub fn read_scan<R: std::io::BufRead + std::io::Seek>(name: &str, source: R) -> Result<Raster> {
    read_scan_limited(name, source, max_samples())
}

/// [`read_scan`], holding at most `max` samples (the tests' small limit exercises the reduction).
fn read_scan_limited<R: std::io::BufRead + std::io::Seek>(name: &str, mut source: R, max: usize) -> Result<Raster> {
    use image::DynamicImage as D;
    use std::io::SeekFrom;
    let fail = |e: &dyn std::fmt::Display| DataError::Format(format!("{name}: {e}"));
    let too_large = || fail(&"the picture is too large to open: save it as a TIFF, which is read at a reduced size");
    // The decoded picture, before its samples become `f32`: as the GeoTIFF reader allows.
    let allowance = (max as u64).saturating_mul(4);
    // The decoders read a file whole before any limit applies, and a picture's file is smaller
    // than its pixels: a larger file is refused unread. (Whatever the extension says, the
    // content is what is decoded.)
    let len = source.seek(SeekFrom::End(0)).map_err(|e| fail(&e))?;
    source.seek(SeekFrom::Start(0)).map_err(|e| fail(&e))?;
    if len > allowance {
        return Err(too_large());
    }
    let mut reader = image::ImageReader::new(source).with_guessed_format().map_err(|e| fail(&e))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(allowance);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| match e {
        image::ImageError::Limits(_) => too_large(),
        e => fail(&e),
    })?;
    let img = if matches!(
        img,
        D::ImageLuma8(_)
            | D::ImageLumaA8(_)
            | D::ImageRgb8(_)
            | D::ImageRgba8(_)
            | D::ImageLuma16(_)
            | D::ImageLumaA16(_)
            | D::ImageRgb16(_)
            | D::ImageRgba16(_)
    ) {
        img
    } else {
        D::ImageRgba8(img.into_rgba8())
    };
    let (w, h) = (img.width(), img.height());
    let ch = usize::from(img.color().channel_count());
    let mut k = 1u32;
    while (w.div_ceil(k) as usize).saturating_mul(h.div_ceil(k) as usize).saturating_mul(ch) > max {
        k = k.saturating_mul(2);
        if k > 1 << 20 {
            return Err(too_large());
        }
    }
    let (bands, color, data_type) = match &img {
        D::ImageLuma8(i) => (deinterleave(i.as_raw(), w, h, ch, k)?, ColorHint::Gray, "UInt8"),
        D::ImageLumaA8(i) => (deinterleave(i.as_raw(), w, h, ch, k)?, ColorHint::GrayAlpha, "UInt8"),
        D::ImageRgb8(i) => (deinterleave(i.as_raw(), w, h, ch, k)?, ColorHint::Rgb { alpha: false }, "UInt8"),
        D::ImageRgba8(i) => (deinterleave(i.as_raw(), w, h, ch, k)?, ColorHint::Rgb { alpha: true }, "UInt8"),
        D::ImageLuma16(i) => (deinterleave(i.as_raw(), w, h, ch, k)?, ColorHint::Gray, "UInt16"),
        D::ImageLumaA16(i) => (deinterleave(i.as_raw(), w, h, ch, k)?, ColorHint::GrayAlpha, "UInt16"),
        D::ImageRgb16(i) => (deinterleave(i.as_raw(), w, h, ch, k)?, ColorHint::Rgb { alpha: false }, "UInt16"),
        D::ImageRgba16(i) => (deinterleave(i.as_raw(), w, h, ch, k)?, ColorHint::Rgb { alpha: true }, "UInt16"),
        _ => return Err(fail(&"this pixel format isn't supported")),
    };
    drop(img);
    let (ow, oh) = (w.div_ceil(k), h.div_ceil(k));
    let mut r = Raster::new(name, ow, oh, bands, unreferenced(ow, oh), Crs::LonLat, None, color, data_type)?;
    r.georeferenced = false;
    r.overview = (k > 1).then_some((w, h));
    Ok(r)
}

/// A picture (PNG or JPEG) laid over a longitude/latitude box (west, south, east, north) and
/// turned `rotation` degrees counter-clockwise about its centre: an RGBA raster, as a KML ground
/// overlay is. Pictures are at most 4096 pixels a side, turned ones at most 32 million pixels.
pub fn from_image(name: &str, bytes: &[u8], bbox: [f64; 4], rotation: f64) -> Result<Raster> {
    let r = from_image_unrotated(name, bytes, bbox)?;
    if !rotation.is_finite() || rotation.abs() < 1e-9 {
        return Ok(r);
    }
    rotate(&r, rotation)
}

/// A raster turned about its centre (in its own coordinates): the turned rectangle's bounding box,
/// at the same pixel size, its corners transparent (nearest pixel).
fn rotate(r: &Raster, degrees: f64) -> Result<Raster> {
    let (w, h) = (f64::from(r.width), f64::from(r.height));
    let (sin, cos) = degrees.to_radians().sin_cos();
    // The pixel grid is square in the picture; a turn in coordinates is a turn of the pixels
    // scaled by the cell's aspect.
    let (px, py) = (r.transform.pixel_width, -r.transform.pixel_height);
    let (cx, cy) = (r.transform.origin_x + w * px / 2.0, r.transform.origin_y - h * py / 2.0);
    let corners =
        [(-w / 2.0 * px, -h / 2.0 * py), (w / 2.0 * px, -h / 2.0 * py), (w / 2.0 * px, h / 2.0 * py), (-w / 2.0 * px, h / 2.0 * py)];
    let turned: Vec<(f64, f64)> = corners.iter().map(|(x, y)| (x * cos - y * sin, x * sin + y * cos)).collect();
    let (min_x, max_x) = turned.iter().fold((f64::MAX, f64::MIN), |(a, b), (x, _)| (a.min(*x), b.max(*x)));
    let (min_y, max_y) = turned.iter().fold((f64::MAX, f64::MIN), |(a, b), (_, y)| (a.min(*y), b.max(*y)));
    let ow = ((max_x - min_x) / px).ceil().clamp(1.0, 16384.0) as u32;
    let oh = ((max_y - min_y) / py).ceil().clamp(1.0, 16384.0) as u32;
    let n = ow as usize * oh as usize;
    if n > 32 << 20 {
        return Err(DataError::Format(format!("{}: the turned picture is too large ({ow} × {oh} pixels)", r.name)));
    }
    let mut bands = (0..r.bands.len()).map(|_| filled(ow as usize, oh as usize, 0.0f32)).collect::<Result<Vec<_>>>()?;
    for row in 0..oh {
        for col in 0..ow {
            // The output pixel's centre, turned back into the picture.
            let x = min_x + (f64::from(col) + 0.5) * px;
            let y = max_y - (f64::from(row) + 0.5) * py;
            let (sx, sy) = (x * cos + y * sin, -x * sin + y * cos);
            let (c, rr) = ((sx / px + w / 2.0).floor(), (h / 2.0 - sy / py).floor());
            if c < 0.0 || rr < 0.0 || c >= w || rr >= h {
                continue;
            }
            let src = rr as usize * r.width as usize + c as usize;
            let dst = row as usize * ow as usize + col as usize;
            for (out, b) in bands.iter_mut().zip(&r.bands) {
                if let (Some(o), Some(v)) = (out.get_mut(dst), b.data.get(src)) {
                    *o = *v;
                }
            }
        }
    }
    let transform = GeoTransform { origin_x: cx + min_x, origin_y: cy + max_y, pixel_width: px, pixel_height: -py };
    Raster::new(r.name.clone(), ow, oh, bands, transform, r.crs, None, r.color.clone(), r.data_type.clone())
}

/// A `w`×`h` image placed by a rotated or skewed `affine` (GDAL's order: x, x per column, x per
/// row, y, y per column, y per row), resampled (nearest pixel) to the north-up grid of square
/// pixels over its bounding box: pixels of the same area, coarser if that would hold more than
/// `max` samples. Outside the image is NaN (transparent). Returns the bands, their size and grid.
fn north_up(bands: Vec<Vec<f32>>, w: u32, h: u32, affine: [f64; 6], max: usize) -> Result<(Vec<Vec<f32>>, u32, u32, GeoTransform)> {
    let [x0, xc, xr, y0, yc, yr] = affine;
    let det = xc * yr - xr * yc;
    if !affine.iter().all(|v| v.is_finite()) || !det.is_normal() {
        return Err(DataError::Format("the raster has no usable georeferencing".into()));
    }
    let (fw, fh) = (f64::from(w), f64::from(h));
    let corners = [(0.0, 0.0), (fw, 0.0), (0.0, fh), (fw, fh)].map(|(c, r)| (x0 + c * xc + r * xr, y0 + c * yc + r * yr));
    let (min_x, max_x) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), (x, _)| (a.min(*x), b.max(*x)));
    let (min_y, max_y) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), (_, y)| (a.min(*y), b.max(*y)));
    let nbands = bands.len().max(1);
    let mut cell = det.abs().sqrt();
    let size = |cell: f64| (((max_x - min_x) / cell).ceil().max(1.0), ((max_y - min_y) / cell).ceil().max(1.0));
    let (mut ow, mut oh) = size(cell);
    // A strong skew spreads the image over a much larger box: coarser pixels keep it in bounds,
    // each step at least 1% coarser so a box rounded up past the limit still shrinks.
    for _ in 0..64 {
        let n = ow * oh * nbands as f64;
        if n <= max as f64 && ow <= f64::from(u32::MAX) && oh <= f64::from(u32::MAX) {
            break;
        }
        cell *= (n / max.max(1) as f64).sqrt().max(1.01);
        (ow, oh) = size(cell);
    }
    if ow * oh * nbands as f64 > max as f64 {
        return Err(DataError::Format(format!("{w} × {h} pixels turned is too large to open")));
    }
    let (ow, oh) = (ow as u32, oh as u32);
    let mut out = (0..bands.len()).map(|_| filled(ow as usize, oh as usize, f32::NAN)).collect::<Result<Vec<_>>>()?;
    for row in 0..oh {
        let y = max_y - (f64::from(row) + 0.5) * cell - y0;
        for col in 0..ow {
            let x = min_x + (f64::from(col) + 0.5) * cell - x0;
            // The output pixel's centre, back in the image's columns and rows.
            let (c, r) = (((yr * x - xr * y) / det).floor(), ((xc * y - yc * x) / det).floor());
            if !(0.0..fw).contains(&c) || !(0.0..fh).contains(&r) {
                continue;
            }
            let src = r as usize * w as usize + c as usize;
            let dst = row as usize * ow as usize + col as usize;
            for (o, b) in out.iter_mut().zip(&bands) {
                if let (Some(o), Some(v)) = (o.get_mut(dst), b.get(src)) {
                    *o = *v;
                }
            }
        }
    }
    Ok((out, ow, oh, GeoTransform { origin_x: min_x, origin_y: max_y, pixel_width: cell, pixel_height: -cell }))
}

fn from_image_unrotated(name: &str, bytes: &[u8], bbox: [f64; 4]) -> Result<Raster> {
    let [w, s, e, n] = bbox;
    if !(w < e && s < n) {
        return Err(DataError::Format(format!("{name}: an empty box")));
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| DataError::Format(format!("{name}: {e}")))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(128 << 20);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| DataError::Format(format!("{name}: {e}")))?.into_rgba8();
    let (width, height) = img.dimensions();
    let mut bands: Vec<Vec<f32>> = (0..4).map(|_| Vec::with_capacity(width as usize * height as usize)).collect();
    for px in img.pixels() {
        for (b, v) in bands.iter_mut().zip(px.0) {
            b.push(f32::from(v));
        }
    }
    let transform =
        GeoTransform { origin_x: w, origin_y: n, pixel_width: (e - w) / f64::from(width), pixel_height: -(n - s) / f64::from(height) };
    Raster::new(name, width, height, bands, transform, Crs::LonLat, None, ColorHint::Rgb { alpha: true }, "UInt8")
}

/// Reads a raster from a file's name and bytes (a browser upload).
pub fn read_bytes(file_name: &str, bytes: &[u8]) -> Result<Raster> {
    let p = Path::new(file_name);
    let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("raster").to_string();
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        // The parts of a large GeoTIFF the web app read (see `geotiff::read_sparse`).
        "tif" | "tiff" if cog::Sparse::is_container(bytes) => {
            let parts = cog::Sparse::from_container(bytes).ok_or_else(|| DataError::Format("a damaged GeoTIFF part file".into()))?;
            match geotiff::read_saved_parts(&parts, &name)? {
                geotiff::Partial::Done(r) => Ok(*r),
                geotiff::Partial::Need(..) => Err(DataError::Format("the parts of the GeoTIFF kept don't hold the whole overview".into())),
            }
        }
        "tif" | "tiff" => geotiff::read(std::io::Cursor::new(bytes), &name),
        "png" | "jpg" | "jpeg" => read_scan(&name, std::io::Cursor::new(bytes)),
        "asc" => {
            let text = std::str::from_utf8(bytes).map_err(|_| DataError::Format("not a text file".into()))?;
            ascii::read(text, &name, None)
        }
        other => Err(DataError::Unsupported(format!(".{other}"))),
    }
}

/// An ASCII grid at `path` (its `text`), in the coordinate system of the `.prj` next to it.
fn read_ascii(path: &Path, text: &str, name: &str, nodata: Option<f64>) -> Result<Raster> {
    let (crs, note) = sidecar_crs(path)?;
    let mut r = ascii::read_with_nodata(text, name, crs, nodata)?;
    // Planetary body notices have their own path. Preserve the method warning without duplicating
    // those, and only assume lon/lat for an unreadable definition.
    r.lost.extend(note.map(|message| match crs {
        Some(_) => format!("the .prj file: {message}"),
        None => format!("the .prj file: {message}; coordinates are read as longitude/latitude"),
    }));
    Ok(r)
}

/// The coordinate system and WKT notice in the `.prj` next to an ASCII grid. An unreadable
/// definition returns no system and explains why, so the grid says it assumes longitude/latitude. Another body's system RustGIS
/// doesn't understand is an error instead, because the grid would land in the wrong place as
/// longitude/latitude.
fn sidecar_crs(path: &Path) -> Result<(Option<Crs>, Option<String>)> {
    let Some(prj) = read_prj(path) else {
        // One that is there but can't be read as text (not UTF-8, too large to be one) is said too.
        let there = std::fs::metadata(path.with_extension("prj")).is_ok_and(|m| m.is_file());
        return Ok((None, there.then(|| "it can't be read as text".to_string())));
    };
    match wkt::read_wkt(&prj) {
        Ok((crs, note)) => {
            // The raster's body channel reports planetary interpretation once; datum and
            // projection notices from this sidecar must otherwise be preserved.
            let note = if crs.is_planetary() { wkt::projection_compatibility_note(&prj, crs).map(str::to_owned) } else { note };
            Ok((Some(crs), note))
        }
        Err(e) => match planetary::refused_wkt(&prj, &e) {
            Some(refused) => Err(DataError::Format(format!("the .prj file: {refused}"))),
            None => Ok((None, Some(e))),
        },
    }
}

/// The text of the `.prj` next to a file (`None` when there's none, or it's too large to be one).
pub fn read_prj(path: &Path) -> Option<String> {
    let prj = path.with_extension("prj");
    let meta = std::fs::metadata(&prj).ok()?;
    if !meta.is_file() || meta.len() > 1 << 20 {
        return None;
    }
    std::fs::read_to_string(prj).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A large band's statistics, counted on any number of threads, are those of one pass on
    /// one thread (#1252) to the bit: the sums in the band's order, the counts, range, histogram
    /// and distinct values whatever the runs. No-data, NaN, −0 and 0 (the first kept), byte-like
    /// values, 257 small whole values, more than 256 whole values, runs that each have at most
    /// 256 but more together, and fractions. The thread count is forced, so the merge is tested on
    /// a machine with one core too.
    #[test]
    fn band_stats_on_several_threads_are_one_threads() {
        // The statistics as one pass on one thread made them before #1252 (the first of equal
        // values kept in the range).
        fn one_pass(data: &[f32], valid: impl Fn(f32) -> bool) -> BandStats {
            let (mut n, mut min, mut max, mut sum, mut sum2) = (0usize, f64::INFINITY, f64::NEG_INFINITY, 0.0f64, 0.0f64);
            let mut whole = true;
            for &v in data.iter().filter(|&&v| valid(v)) {
                let x = v as f64;
                n += 1;
                if x < min {
                    min = x;
                }
                if x > max {
                    max = x;
                }
                sum += x;
                sum2 += x * x;
                whole &= x.fract() == 0.0;
            }
            if n == 0 {
                return BandStats::default();
            }
            let mean = sum / n as f64;
            let std_dev = (sum2 / n as f64 - mean * mean).max(0.0).sqrt();
            let span = max - min;
            let k = (HISTOGRAM_BINS as f64 - 1.0) / if span > 0.0 { span } else { 1.0 };
            let mut histogram = vec![0u64; HISTOGRAM_BINS];
            let mut distinct = std::collections::BTreeSet::new();
            for &v in data.iter().filter(|&&v| valid(v)) {
                let x = v as f64;
                histogram[(((x - min) * k).round() as usize).min(HISTOGRAM_BINS - 1)] += 1;
                if whole {
                    distinct.insert(x as i64);
                }
            }
            let categories = (whole && distinct.len() <= 256).then(|| distinct.into_iter().map(|v| v as f64).collect());
            BandStats { count: n, min, max, mean, std_dev, histogram, categories }
        }
        let n = 2_000_003usize;
        let at = |i: usize| ((i as u64 * 7919 + i as u64 / 1013 * 31) % 100_003) as usize;
        let bands: [(&str, Vec<f32>); 7] = [
            ("bytes", (0..n).map(|i| (at(i) % 256) as f32).collect()),
            ("257 small whole", (0..n).map(|i| (at(i) % 257) as f32).collect()),
            ("few whole", (0..n).map(|i| (at(i) % 200) as f32 * 3.0 - 250.0).collect()),
            // Each half has 200 values, together 400: two runs keep theirs, the merge gives up.
            ("halves", (0..n).map(|i| -((at(i) % 200) as f32) - if i < n / 2 { 1.0 } else { 201.0 }).collect()),
            ("wide whole", (0..n).map(|i| at(i) as f32 - 7.0).collect()),
            ("fractions", (0..n).map(|i| at(i) as f32 * 0.013 - 400.0).collect()),
            ("gaps", (0..n).map(|i| [f32::NAN, -0.0, 0.0, -9999.0, 17.5][at(i) % 5]).collect()),
        ];
        let bits = |s: &BandStats| (s.count, s.min.to_bits(), s.max.to_bits(), s.mean.to_bits(), s.std_dev.to_bits());
        let valid = |v: f32| v.is_finite() && v != -9999.0;
        for (name, data) in &bands {
            let want = one_pass(data, valid);
            for threads in [1, 2, 7, 16] {
                let got = BandStats::compute_on(data, valid, None, threads);
                assert_eq!(bits(&got), bits(&want), "{name} on {threads}");
                assert_eq!(got.histogram, want.histogram, "{name} on {threads}");
                assert_eq!(got.categories, want.categories, "{name} on {threads}");
            }
        }
        assert!(BandStats::compute_on(&bands[1].1, valid, None, 2).categories.is_none(), "257 small whole values");
        assert!(BandStats::compute_on(&bands[3].1, valid, None, 2).categories.is_none(), "400 in two runs of 200");
        // A band too small for threads, and one with no valid value.
        let small: Vec<f32> = (0..1000).map(|i| (i % 37) as f32).collect();
        assert_eq!(BandStats::from_values(&small), one_pass(&small, f32::is_finite));
        assert_eq!(BandStats::compute_on(&vec![f32::NAN; n], valid, None, 4), BandStats::default());
    }

    /// A band the machine can't hold is an error naming its size, not the end of the program
    /// (#1291), on 64-bit (the reservation fails) and wasm32 (the size overflows) alike; one it can
    /// is the size asked for.
    #[test]
    fn a_band_too_large_to_hold_is_an_error() {
        let huge = 1usize << 31;
        let err = grid::<f32>(huge, huge).unwrap_err().to_string();
        assert_eq!(err, format!("not enough memory for a {huge} × {huge} raster"));
        assert!(filled(usize::MAX, 2, 0u8).is_err());
        assert!(room::<f64>(usize::MAX / 2, 1, 1).is_err(), "too many bytes to ask for");
        assert_eq!(filled(3, 2, f32::NAN).map(|v| v.len()).ok(), Some(6));
        assert!(grid::<u16>(4, 5).is_ok_and(|g| g.is_empty() && g.capacity() >= 20));
        assert!(collected(huge, huge, std::iter::repeat(0.0f32)).is_err());
        assert!(collected(3, 2, 0..7).is_err(), "more than w × h values");
        assert_eq!(collected(3, 2, 0..6).ok(), Some(vec![0, 1, 2, 3, 4, 5]));
        assert_eq!(collected(3, 2, 0..4).ok(), Some(vec![0, 1, 2, 3]));
    }

    /// A PNG of `w`×`h` pixels, red in its top half and blue below.
    pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
        let img =
            image::RgbaImage::from_fn(w, h, |_, y| if y < h / 2 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 255, 255]) });
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn pictures_over_boxes() {
        let r = from_image("o", &png(20, 10), [10.0, 40.0, 12.0, 41.0], 0.0).unwrap();
        assert_eq!((r.width, r.height, r.bands.len()), (20, 10, 4));
        let e = r.extent();
        assert_eq!((e.min_x, e.min_y, e.max_x, e.max_y), (10.0, 40.0, 12.0, 41.0));
        assert_eq!((r.bands[0].data[0], r.bands[2].data[199]), (255.0, 255.0), "red on top, blue below");
        // A quarter turn: the box's centre stays, it is as tall as it was wide, and its corners
        // (outside the picture) are see-through.
        let t = from_image("o", &png(20, 10), [10.0, 40.0, 12.0, 41.0], 90.0).unwrap();
        let e = t.extent();
        assert!(((e.min_x + e.max_x) / 2.0 - 11.0).abs() < 0.11 && ((e.min_y + e.max_y) / 2.0 - 40.5).abs() < 0.11, "{e:?}");
        assert!(((e.max_y - e.min_y) - 2.0).abs() < 0.11 && ((e.max_x - e.min_x) - 1.0).abs() < 0.11, "{e:?}");
        let small = from_image("o", &png(20, 10), [10.0, 40.0, 12.0, 41.0], 30.0).unwrap();
        assert_eq!(small.bands[3].data[0], 0.0, "a turned picture's corner is transparent");
        assert!(from_image("o", b"not a picture", [0.0, 0.0, 1.0, 1.0], 0.0).is_err());
        assert!(from_image("o", &png(2, 2), [1.0, 0.0, 0.0, 1.0], 0.0).is_err(), "an empty box");
    }

    /// A 4×3 north-up lon/lat raster with one no-data pixel.
    pub(crate) fn small() -> Raster {
        let gt = GeoTransform { origin_x: 10.0, origin_y: 50.0, pixel_width: 0.5, pixel_height: -0.5 };
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, -9999.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0];
        Raster::new("small", 4, 3, vec![data], gt, Crs::LonLat, Some(-9999.0), ColorHint::Gray, "Float32").unwrap()
    }

    #[test]
    fn gdal_sidecars_name_bands_and_set_nodata() {
        let dir = std::env::temp_dir().join(format!("rustgis-pam-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gt = GeoTransform { origin_x: 10.0, origin_y: 50.0, pixel_width: 0.5, pixel_height: -0.5 };
        let r = Raster::new("dem", 2, 2, vec![vec![0.0, 5.0, 7.0, 9.0]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let tif = dir.join("dem.tif");
        std::fs::write(&tif, geotiff::write(&r, 0).unwrap()).unwrap();
        let xml = r#"<PAMDataset><PAMRasterBand band="1"><Description>Elevation</Description><NoDataValue>0</NoDataValue></PAMRasterBand></PAMDataset>"#;
        std::fs::write(dir.join("dem.tif.aux.xml"), xml).unwrap();
        let back = read_path(&tif).unwrap();
        assert_eq!((back.bands[0].name.as_str(), back.nodata), ("Elevation", Some(0.0)));
        assert_eq!((back.value(0, 0, 0), back.bands[0].stats.count, back.bands[0].stats.min), (None, 3, 5.0));
        // An ASCII grid's `x.aux.xml`; the file's own no-data value wins.
        let asc = dir.join("grid.asc");
        std::fs::write(&asc, "ncols 2\nnrows 1\nxllcorner 0\nyllcorner 0\ncellsize 1\nNODATA_value -1\n-1 0\n").unwrap();
        std::fs::write(dir.join("grid.aux.xml"), xml).unwrap();
        let g = read_path(&asc).unwrap();
        assert_eq!((g.bands[0].name.as_str(), g.nodata, g.value(0, 1, 0)), ("Elevation", Some(-1.0), Some(0.0)));
        // A broken sidecar is ignored.
        std::fs::write(dir.join("dem.tif.aux.xml"), "<PAMDataset><PAMRasterBand").unwrap();
        assert_eq!(read_path(&tif).unwrap().bands[0].name, "Band 1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gdal_sidecars_name_classes() {
        let dir = std::env::temp_dir().join(format!("rustgis-pam-classes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gt = GeoTransform { origin_x: 10.0, origin_y: 50.0, pixel_width: 0.5, pixel_height: -0.5 };
        let r = Raster::new("slope", 2, 2, vec![vec![1.0, 2.0, 3.0, 1.0]], gt, Crs::LonLat, None, ColorHint::Gray, "UInt8").unwrap();
        let tif = dir.join("slope.tif");
        std::fs::write(&tif, geotiff::write(&r, 0).unwrap()).unwrap();
        let xml = r#"<PAMDataset><PAMRasterBand band="1"><CategoryNames>
<Category>no data</Category><Category>0-5 % flat</Category><Category></Category><Category>over 100 %</Category>
</CategoryNames></PAMRasterBand></PAMDataset>"#;
        std::fs::write(dir.join("slope.tif.aux.xml"), xml).unwrap();
        let back = read_path(&tif).unwrap();
        let b = &back.bands[0];
        assert_eq!(b.classes, ["no data", "0-5 % flat", "", "over 100 %"]);
        assert_eq!((b.class_name(1.0), b.class_name(3.0)), (Some("0-5 % flat"), Some("over 100 %")));
        for v in [2.0, 4.0, 1.5, -1.0, f64::NAN] {
            assert_eq!(b.class_name(v), None, "{v}");
        }
        assert!(small().bands[0].classes.is_empty(), "no sidecar, no names");
        // Names for a band the raster doesn't have are said, not dropped.
        let mut one = small();
        one.apply_pam(
            &pam::Pam {
                bands: vec![pam::PamBand { band: 2, description: Some("NIR".into()), ..Default::default() }],
                lost: vec!["1 band record(s) for no band from 1 to 65536 left out".into()],
            },
            pam::SIDECAR,
        );
        assert!(
            one.lost.iter().any(|l| l == "its .aux.xml sidecar: names or wavelengths for 1 band(s) it doesn't have (it has 1) left out"),
            "{:?}",
            one.lost
        );
        // A no-data value for a band it doesn't have isn't its own, and is said.
        let mut zero = small();
        zero.nodata = None;
        zero.apply_pam(
            &pam::Pam { bands: vec![pam::PamBand { band: 2, nodata: Some(0.0), ..Default::default() }], lost: Vec::new() },
            pam::SIDECAR,
        );
        assert_eq!(zero.nodata, None);
        // Bands that disagree give none, and say so; bands that agree give theirs.
        let mut two = small();
        two.nodata = None;
        let both = |a: f64, b: f64| pam::Pam {
            bands: vec![
                pam::PamBand { band: 1, nodata: Some(a), ..Default::default() },
                pam::PamBand { band: 1, nodata: Some(b), ..Default::default() },
            ],
            lost: Vec::new(),
        };
        two.apply_pam(&both(0.0, 255.0), pam::SIDECAR);
        assert_eq!(two.nodata, None);
        assert_eq!(
            two.lost,
            ["its .aux.xml sidecar: its bands give different no-data values (0, 255): none is applied, so those pixels are read as values"]
        );
        let mut same = small();
        same.nodata = None;
        same.apply_pam(&both(0.0, 0.0), pam::SIDECAR);
        assert!(same.nodata == Some(0.0) && same.lost.is_empty(), "{:?}", same.lost);
        // A file with its own no-data value keeps it; a sidecar giving another says so, one giving
        // the same says nothing.
        let mut own = small();
        own.nodata = Some(-9999.0);
        own.apply_pam(&both(0.0, -9999.0), pam::SIDECAR);
        assert_eq!(own.nodata, Some(-9999.0));
        assert_eq!(own.lost, ["its .aux.xml sidecar: its no-data value (0) isn't the file's own (-9999), which is kept"]);
        let mut agreed = small();
        agreed.nodata = Some(-9999.0);
        agreed.apply_pam(&both(-9999.0, -9999.0), pam::SIDECAR);
        assert!(agreed.lost.is_empty(), "{:?}", agreed.lost);
        assert_eq!(zero.lost, ["its .aux.xml sidecar: a no-data value for 1 band(s) it doesn't have (it has 1) left out"]);
        // What the document left out is said as the sidecar's, not the GDAL tag's.
        assert!(
            one.lost.iter().any(|l| l == "its .aux.xml sidecar: 1 band record(s) for no band from 1 to 65536 left out"),
            "{:?}",
            one.lost
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_statistics_stand_in_for_an_overview() {
        let pam = pam::Pam {
            bands: vec![pam::PamBand {
                band: 1,
                stats: Some(pam::Stats { min: -50.0, max: 900.0, mean: Some(300.0), std_dev: Some(40.0) }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut full = small();
        full.apply_pam(&pam, pam::SIDECAR);
        assert_eq!((full.bands[0].stats.min, full.bands[0].stats.max), (1.0, 12.0), "the whole band's own statistics are exact");
        let mut ov = small();
        ov.overview = Some((400, 300));
        ov.apply_pam(&pam, pam::SIDECAR);
        let s = &ov.bands[0].stats;
        assert_eq!((s.min, s.max, s.mean, s.std_dev, s.count), (-50.0, 900.0, 300.0, 40.0, 11));
        assert_eq!(s.histogram.iter().sum::<u64>(), 11);
    }

    #[test]
    fn stats_extent_and_lookup() {
        let r = small();
        let s = &r.bands[0].stats;
        assert_eq!(s.count, 11);
        assert_eq!((s.min, s.max), (1.0, 12.0));
        assert_eq!(s.categories.as_ref().map(Vec::len), Some(11));
        assert!(s.percentile(50.0) >= 5.0 && s.percentile(50.0) <= 7.0);
        let e = r.extent();
        assert!((e.min_x - 10.0).abs() < 1e-9 && (e.max_x - 12.0).abs() < 1e-9);
        assert!((e.min_y - 48.5).abs() < 1e-9 && (e.max_y - 50.0).abs() < 1e-9);
        assert_eq!(r.value(0, 1, 1), None, "no-data");
        assert_eq!(r.value(0, 2, 1), Some(7.0));
        assert_eq!(r.value(0, -1, 0), None);
        assert_eq!(r.value(9, 0, 0), None);
        assert_eq!(r.pixel_at(Coord::new(10.6, 49.9)), Some((1, 0)));
        assert_eq!(r.pixel_at(Coord::new(0.0, 0.0)), None);
    }

    /// #1109: a lon/lat grid stored past 180° (0 to 360°) covers the places it holds within
    /// ±180°, and the place at a western longitude is found in its columns past 180°.
    #[test]
    fn longitudes_past_180_go_round() {
        let grid = |x0: f64, w: u32, px: f64, crs: Crs| {
            let gt = GeoTransform { origin_x: x0, origin_y: 77.25, pixel_width: px, pixel_height: -0.5 };
            Raster::new("g", w, 309, vec![vec![1.0; w as usize * 309]], gt, crs, None, ColorHint::Gray, "Float32").unwrap()
        };
        let lon = |r: &Raster| (r.extent().min_x, r.extent().max_x);
        // The issue's grid: 720 × 0.5° from -0.25°, in EPSG:4326 and in another geographic system.
        for crs in [Crs::LonLat, Crs::from_epsg(4269).unwrap()] {
            let r = grid(-0.25, 720, 0.5, crs);
            assert_eq!(lon(&r), (-180.0, 180.0), "{crs:?}");
            assert!((r.extent().min_y + 77.25).abs() < 1e-6 && (r.extent().max_y - 77.25).abs() < 1e-6, "{crs:?}");
            assert_eq!(r.pixel_at(Coord::new(-100.0, 0.0)), Some((520, 154)), "{crs:?}: 260°");
            assert_eq!(r.pixel_at(Coord::new(100.0, 0.0)), Some((200, 154)), "{crs:?}");
            assert_eq!(r.pixel_at(Coord::new(-0.1, 0.0)), Some((0, 154)), "{crs:?}");
            assert_eq!(r.pixel_at(Coord::new(-0.3, 0.0)), Some((719, 154)), "{crs:?}: 359.7°");
            // Do not clamp away the eastern strip of a regional grid straddling -180°.
            let west = grid(-180.25, 41, 0.25, crs);
            assert_eq!(lon(&west), (-180.0, 180.0), "{crs:?}");
            assert!(west.extent().contains(Coord::new(179.9, 0.0)), "{crs:?}");
        }
        // A grid a turn east of its place is there; one across the antimeridian goes all round.
        assert_eq!(lon(&grid(190.0, 20, 0.5, Crs::LonLat)), (-170.0, -160.0));
        assert_eq!(lon(&grid(170.0, 40, 0.5, Crs::LonLat)), (-180.0, 180.0));
        // Columns off a grid that doesn't reach round stay off it, and projected grids never wrap.
        let part = grid(190.0, 20, 0.5, Crs::LonLat);
        assert_eq!(part.pixel_at(Coord::new(0.0, 0.0)), None);
        assert_eq!(part.pixel_at(Coord::new(-165.0, 0.0)), Some((10, 154)));
        assert_eq!(grid(-0.25, 720, 0.5, Crs::WebMercator).wrap_col(-1.5), -1.5);
        assert!(grid(-0.25, 720, 0.5, Crs::LonLat).wrap_col(f64::NAN).is_nan());
    }

    #[test]
    fn geographic_extent_uses_greenwich_not_the_native_prime_meridian() {
        let crs = Crs::parse("+proj=longlat +datum=WGS84 +pm=paris +no_defs").unwrap();
        let grid = |x0: f64, width: u32| {
            let gt = GeoTransform { origin_x: x0, origin_y: 1.0, pixel_width: 0.5, pixel_height: -1.0 };
            Raster::new("Paris", width, 2, vec![vec![1.0; width as usize * 2]], gt, crs, None, ColorHint::Gray, "Float32").unwrap()
        };
        let e = grid(190.0, 20).extent();
        let (west, east) = (-167.662_770_833_333, -157.662_770_833_333);
        assert!((e.min_x - west).abs() < 1e-9 && (e.max_x - east).abs() < 1e-9, "{e:?}");
        for x in [west + 1e-8, east - 1e-8] {
            assert!(e.contains(Coord::new(x, 0.0)), "{x}: {e:?}");
        }
        for x in [west - 1e-8, east + 1e-8] {
            assert!(!e.contains(Coord::new(x, 0.0)), "{x}: {e:?}");
        }
        // 177°..178° Paris is 179.337°..180.337° Greenwich, across the real antimeridian.
        let e = grid(177.0, 2).extent();
        assert_eq!((e.min_x, e.max_x), (-180.0, 180.0));
        assert!(e.contains(Coord::new(179.9, 0.0)) && e.contains(Coord::new(-179.9, 0.0)));
    }

    #[test]
    fn rejects_bad_shapes() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 0.0, pixel_width: 1.0, pixel_height: -1.0 };
        assert!(Raster::new("x", 2, 2, vec![vec![0.0; 3]], gt, Crs::LonLat, None, ColorHint::Gray, "").is_err());
        assert!(Raster::new("x", 0, 2, vec![vec![]], gt, Crs::LonLat, None, ColorHint::Gray, "").is_err());
        let bad = GeoTransform { pixel_width: 0.0, ..gt };
        assert!(Raster::new("x", 1, 1, vec![vec![0.0]], bad, Crs::LonLat, None, ColorHint::Gray, "").is_err());
        assert!(read_bytes("x.tif", b"not a tiff").is_err());
        assert!(read_bytes("x.asc", &[0xff, 0xfe]).is_err());
        assert!(read_bytes("x.png", b"").is_err());
    }

    /// #1486: the placement cases, as an ASCII grid's `.prj`: its lower left corner, at the case's
    /// point, is where the case says.
    #[test]
    fn placement_cases_are_placed_from_a_grids_prj() {
        let dir = std::env::temp_dir().join(format!("rustgis-placement-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for wkt::tests::PlacementCase { what, wkt, xy, lonlat } in wkt::tests::placement_cases() {
            let asc = dir.join("grid.asc");
            std::fs::write(&asc, format!("ncols 1\nnrows 1\nxllcorner {}\nyllcorner {}\ncellsize 1\n7\n", xy.x, xy.y)).unwrap();
            std::fs::write(dir.join("grid.prj"), &wkt).unwrap();
            let r = read_path(&asc).unwrap();
            assert!(r.lost.is_empty(), "{what}: {:?}", r.lost);
            // The origin is the upper left corner; `pixel_height` is negative, so this is the lower
            // left.
            let corner = Coord::new(r.transform.origin_x, r.transform.origin_y + r.transform.pixel_height);
            let ll = r.crs.transformer().unwrap().to_lonlat(corner).unwrap();
            let off = wkt::tests::metres_apart(ll, lonlat);
            assert!(off < 0.01, "{what}: {off} m off");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    use std::io::Cursor;

    fn tiff(tags: &[(u16, &[f64])]) -> Vec<u8> {
        use tiff::encoder::{TiffEncoder, colortype};
        let mut out = Cursor::new(Vec::new());
        let mut enc = TiffEncoder::new(&mut out).unwrap();
        let mut img = enc.new_image::<colortype::RGB8>(5, 3).unwrap();
        for (tag, v) in tags {
            img.encoder().write_tag(tiff::tags::Tag::Unknown(*tag), *v).unwrap();
        }
        img.write_data(&(0..45).collect::<Vec<u8>>()).unwrap();
        out.into_inner()
    }

    fn encode(img: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    fn picture(w: u32, h: u32, alpha: bool) -> image::DynamicImage {
        if alpha {
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([x as u8, y as u8, 7, 200])))
        } else {
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 7])))
        }
    }

    fn assert_scan(r: &Raster, size: (u32, u32), bands: usize, color: ColorHint) {
        assert_eq!((r.width, r.height, r.bands.len()), (size.0, size.1, bands));
        assert_eq!(r.color, color);
        assert!(!r.georeferenced);
        assert!(!r.extent().is_empty());
        assert!(r.transform.is_valid());
    }

    #[test]
    fn a_tiff_without_tags_opens_as_a_scan() {
        let r = read_bytes("scan.tif", &tiff(&[])).unwrap();
        assert_scan(&r, (5, 3), 3, ColorHint::Rgb { alpha: false });
        assert_eq!(r.bands[1].data.get(1).copied(), Some(4.0));
        // Part of the GeoTIFF tags is a damaged GeoTIFF, not a scan.
        assert!(read_bytes("x.tif", &tiff(&[(33550, &[1.0, 1.0, 0.0])])).is_err());
        // A real GeoTIFF is read as before.
        let g = read_bytes("g.tif", &geotiff::tests_support::strip_tiff()).unwrap();
        assert!(g.georeferenced);
    }

    #[test]
    fn png_and_jpeg_open_as_scans() {
        let png = read_bytes("a.png", &encode(picture(7, 4, true), image::ImageFormat::Png)).unwrap();
        assert_scan(&png, (7, 4), 4, ColorHint::Rgb { alpha: true });
        assert_eq!((png.data_type.as_str(), png.bands[0].data.get(3).copied()), ("UInt8", Some(3.0)));
        let jpeg = read_bytes("b.JPG", &encode(picture(16, 9, false), image::ImageFormat::Jpeg)).unwrap();
        assert_scan(&jpeg, (16, 9), 3, ColorHint::Rgb { alpha: false });
        let grey =
            encode(image::DynamicImage::ImageLuma16(image::ImageBuffer::from_pixel(3, 2, image::Luma([40_000]))), image::ImageFormat::Png);
        let grey = read_bytes("c.png", &grey).unwrap();
        assert_scan(&grey, (3, 2), 1, ColorHint::Gray);
        assert_eq!((grey.data_type.as_str(), grey.bands[0].data.first().copied()), ("UInt16", Some(40_000.0)));
    }

    #[test]
    fn a_scan_is_placed_over_a_box_and_stays_a_scan() {
        let mut r = read_bytes("a.png", &encode(picture(20, 10, false), image::ImageFormat::Png)).unwrap();
        r.place([10.0, 40.0, 12.0, 41.0]).unwrap();
        assert!(!r.georeferenced);
        let e = r.extent();
        assert!((e.min_x - 10.0).abs() < 1e-9 && (e.max_y - 41.0).abs() < 1e-9);
        assert!(r.place([1.0, 1.0, 0.0, 2.0]).is_err() && r.place([0.0, f64::NAN, 1.0, 2.0]).is_err());
        assert!(from_image("o", &encode(picture(4, 4, true), image::ImageFormat::Png), [0.0, 0.0, 1.0, 1.0], 0.0).unwrap().georeferenced);
    }

    #[test]
    fn a_scan_has_no_coordinates_to_write() {
        let mut r = read_bytes("a.png", &encode(picture(8, 4, false), image::ImageFormat::Png)).unwrap();
        let e = geotiff::write(&r, 0).unwrap_err().to_string();
        assert!(e.contains("georeference it first"), "{e}");
        r.place([10.0, 40.0, 12.0, 41.0]).unwrap();
        assert!(geotiff::write(&r, 0).is_err());
        // A GeoTIFF forced to be a scan loses its coordinates; one that has them still writes.
        let g = read_bytes("g.tif", &geotiff::tests_support::strip_tiff()).unwrap();
        assert!(geotiff::write(&g, 0).is_ok());
        let s = g.into_scan().unwrap();
        assert!(!s.georeferenced && s.require_coordinates().is_err() && geotiff::write(&s, 0).is_err());
    }

    #[test]
    fn a_large_scan_is_read_at_every_other_pixel() {
        let png = encode(picture(40, 30, false), image::ImageFormat::Png);
        let r = read_scan_limited("big", Cursor::new(&png), 900).unwrap();
        assert_scan(&r, (20, 15), 3, ColorHint::Rgb { alpha: false });
        assert_eq!(r.overview, Some((40, 30)));
        // Past the decoded-size allowance it is refused rather than allocated, in RustGIS's words.
        let e = read_scan_limited("big", Cursor::new(&png), 100).unwrap_err().to_string();
        assert!(e.contains("save it as a TIFF"), "{e}");
        // A file larger than the allowance is refused before it is read (whatever its extension).
        let jpeg = encode(picture(40, 30, false), image::ImageFormat::Jpeg);
        let e = read_scan_limited("x.png", Cursor::new(&jpeg), jpeg.len() / 8).unwrap_err().to_string();
        assert!(e.contains("save it as a TIFF"), "{e}");
        assert!(read_scan_limited("x.png", Cursor::new(&jpeg), 900).is_ok());
    }

    #[test]
    fn damaged_scans_are_errors() {
        let files = [
            ("a.tif", tiff(&[])),
            ("a.png", encode(picture(30, 20, true), image::ImageFormat::Png)),
            ("a.jpg", encode(picture(30, 20, false), image::ImageFormat::Jpeg)),
        ];
        for (name, bytes) in files {
            for cut in [0, 1, 8, bytes.len() / 2, bytes.len() - 1] {
                // Whatever a cut leaves is read or refused, never a panic.
                let _ = read_bytes(name, bytes.get(..cut).unwrap_or_default());
            }
            assert!(read_bytes(name, bytes.get(..bytes.len() / 3).unwrap_or_default()).is_err(), "{name}");
            assert!(read_bytes(name, &[0xAB; 64]).is_err(), "{name}");
            assert!(read_bytes(name, b"").is_err(), "{name}");
        }
        // A header claiming a huge picture, with no pixels behind it (and a CRC that checks out,
        // so only the size limit refuses it).
        let mut png = encode(picture(2, 2, true), image::ImageFormat::Png);
        if let Some(ihdr) = png.get_mut(16..24) {
            ihdr.copy_from_slice(&[0, 0, 0xEA, 0x60, 0, 0, 0xEA, 0x60]);
        }
        let crc = png.get(12..29).unwrap_or_default().iter().fold(!0u32, |mut c, b| {
            c ^= u32::from(*b);
            for _ in 0..8 {
                c = if c & 1 == 1 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            c
        });
        if let Some(at) = png.get_mut(29..33) {
            at.copy_from_slice(&(!crc).to_be_bytes());
        }
        let message = read_bytes("huge.png", &png).unwrap_err().to_string();
        assert!(message.contains("save it as a TIFF"), "{message}");
    }
}

/// Reads every raster in `$RUSTGIS_SAMPLE_RASTERS` (sample files from
/// <https://source.coop/giswqs/opengeos>): `cargo test -p rustgis-data -- --ignored samples`.
#[cfg(test)]
#[test]
#[ignore]
fn samples() {
    let Ok(dir) = std::env::var("RUSTGIS_SAMPLE_RASTERS") else { return };
    let mut paths: Vec<_> =
        std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| is_raster_name(&p.to_string_lossy())).collect();
    paths.sort();
    assert!(!paths.is_empty());
    for p in paths {
        let t = std::time::Instant::now();
        let r = match read_path(&p) {
            Ok(r) => r,
            Err(e) => {
                println!("FAILED {}: {e}", p.display());
                continue;
            }
        };
        let s = &r.bands[0].stats;
        println!(
            "{}: {}×{}×{} {} {} {:?} nodata={:?} band1 [{}, {}] categories={:?} extent={:?} in {:?}",
            r.name,
            r.width,
            r.height,
            r.band_count(),
            r.data_type,
            r.crs.label(),
            r.color,
            r.nodata,
            s.min,
            s.max,
            s.categories.as_ref().map(Vec::len),
            r.extent(),
            t.elapsed()
        );
    }
}
