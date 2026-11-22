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
pub mod crs_search;
mod decoded;
pub mod dted;
#[cfg(not(target_arch = "wasm32"))]
mod file_revision;
#[cfg(not(target_arch = "wasm32"))]
pub mod gdal;
pub mod geotiff;
pub mod grib;
pub mod hgt;
mod image_grid;
mod lerc;
mod lzw;
mod mask;
pub mod pam;
#[cfg(feature = "pdf")]
pub mod pdf;
pub mod planetary;
pub mod reproject;
pub mod wkt;
pub mod world;
pub mod zarr;

use std::path::Path;

pub use crs::{Crs, Transformer};
pub use image_grid::ImageGrid;
pub use reproject::Reprojection;
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
/// Less once RustGIS holds more than half the memory, never below the floor ([`crate::memory::planned`], #2431).
pub fn max_samples() -> usize {
    crate::memory::planned(SAMPLES_FLOOR, SAMPLES_CEILING, 8)
}

// Shared by the elevation readers ([`hgt`], [`dted`]) and pictures read at a step.

/// The transform of a grid of posts (samples on grid lines) whose south-west post is at
/// (`west`, `south`), `cols` × `rows` of them `dx` and `dy` degrees apart, read every `step`-th
/// post from the north-west one: each post kept becomes a cell centered on it, `step` posts wide,
/// so the grid's edges lie half a cell beyond the outer posts kept. (At `step` 1, half a post's
/// spacing beyond the outer posts.)
fn post_transform(west: f64, south: f64, rows: usize, dx: f64, dy: f64, step: usize) -> GeoTransform {
    let north = south + (rows.saturating_sub(1)) as f64 * dy;
    let k = step.max(1) as f64;
    let (cell_x, cell_y) = (dx * k, dy * k);
    GeoTransform { origin_x: west - cell_x / 2.0, origin_y: north + cell_y / 2.0, pixel_width: cell_x, pixel_height: -cell_y }
}

/// The step at which a `w` × `h` grid fits in `max` samples: 1, or the smallest power of two
/// whose every-`step`-th sample does ([`read_scan`] reduces pictures with it too). `None`
/// when none does, up to a step of 2^20.
fn step_within(w: usize, h: usize, max: usize) -> Option<usize> {
    let mut k = 1usize;
    while w.div_ceil(k).saturating_mul(h.div_ceil(k)) > max {
        k = k.checked_mul(2)?;
        if k > 1 << 20 {
            return None;
        }
    }
    Some(k)
}

/// What a tile says of its heights' vertical reference, once a layer (a raster's `lost`): the
/// layer's coordinate system is horizontal only, so this is where it is said.
fn vertical_note(surface: &str) -> String {
    format!(
        "heights are meters above {surface}, not the WGS 84 ellipsoid; the layer's coordinate system doesn't carry a vertical datum, so compare it with ellipsoidal heights (GPS, many lidar files) only after a geoid correction"
    )
}

/// What reading a grid's no-data posts says: `None` when there are none.
fn void_note(voids: u64, total: u64, value: i16) -> Option<String> {
    match voids {
        0 => None,
        1 => Some(format!("1 of its {total} heights is a void ({value}), read as no data")),
        n => Some(format!("{n} of its {total} heights are voids ({value}), read as no data")),
    }
}

/// Largest ASCII grid file read (1 GiB).
pub const MAX_ASCII_BYTES: u64 = 1 << 30;
/// File extensions read as rasters. SRTM height tiles (`.hgt`) and DTED (`.dt0`–`.dt2`) are
/// read natively ([`hgt`], [`dted`]), on the desktop and in the browser: the last four are
/// [`ELEVATION_EXTENSIONS`] (a test holds them to it).
pub const READ_EXTENSIONS: &[&str] =
    &["tif", "tiff", "asc", "png", "jpg", "jpeg", "grb", "grib", "grb2", "grib2", "hgt", "dt0", "dt1", "dt2"];
/// The extensions of the elevation files read natively: SRTM height tiles and DTED levels 0–2.
pub const ELEVATION_EXTENSIONS: [&str; 4] = ["hgt", "dt0", "dt1", "dt2"];

/// Whether a file name is an SRTM height tile's or a DTED file's ([`ELEVATION_EXTENSIONS`]).
pub fn is_elevation_name(name: &str) -> bool {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    ELEVATION_EXTENSIONS.contains(&ext.as_str())
}

/// Whether a file name is a raster this module reads: a PDF's page too (refused with
/// [`PDF_ELSEWHERE`] where [`PDF_PAGES`] is false).
pub fn is_raster_name(name: &str) -> bool {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    READ_EXTENSIONS.contains(&ext.as_str()) || ext == "pdf"
}

/// Whether a file name is a PDF's (`.pdf`): its pages are read as rasters ([`PDF_PAGES`]).
pub fn is_pdf_name(name: &str) -> bool {
    Path::new(name).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Whether this build reads PDF pages (the `pdf` feature, #2336).
pub const PDF_PAGES: bool = cfg!(feature = "pdf");

/// Why a PDF isn't read in a build without the `pdf` feature (the browser, phones).
pub const PDF_ELSEWHERE: &str = "PDFs are read in the desktop app, the command line and Python, not here: open it there";

/// Reads page `page` of a PDF file (its number; the first when `None`), placed by its coordinates
/// when `place` (a scan otherwise), or says it isn't read here.
fn read_pdf_page(path: &Path, page: Option<&str>, place: bool) -> Result<Raster> {
    #[cfg(feature = "pdf")]
    return pdf::read_file(path, page, place);
    #[cfg(not(feature = "pdf"))]
    {
        let _ = (path, page, place);
        Err(DataError::Unsupported(PDF_ELSEWHERE.into()))
    }
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
        BandStats::compute_with(data, |_, v| v.is_finite(), None)
    }

    /// Statistics of a raster's band `data`: the values `valid` takes, less, for a colour band of
    /// a raster with alpha, the cells its alpha band (`alpha`) draws see-through ([`see_through`],
    /// an alpha of `opaque` being opaque). `stored` is the band's whole statistics, for a reduced
    /// overview ([`BandStats::compute_with`]); GDAL counts see-through cells in all but 8-bit
    /// images, so a band with `alpha` doesn't take them.
    fn of_band(
        data: &[f32],
        valid: impl Fn(f32) -> bool + Sync,
        alpha: Option<&[f32]>,
        opaque: f32,
        stored: Option<pam::Stats>,
    ) -> BandStats {
        match alpha {
            Some(alpha) => {
                BandStats::compute_with(data, |i, v| valid(v) && alpha.get(i).is_some_and(|&a| !see_through(a, valid(a), opaque)), None)
            }
            None => BandStats::compute_with(data, |_, v| valid(v), stored),
        }
    }

    /// Statistics of the values of `data` that `valid` takes (given each one's place in `data` and
    /// its value), taking the range, mean and spread from `stored` (statistics of the whole band,
    /// when `data` is a reduced overview of it).
    fn compute_with(data: &[f32], valid: impl Fn(usize, f32) -> bool + Sync, stored: Option<pam::Stats>) -> BandStats {
        BandStats::compute_on(data, valid, stored, threads_for(data.len()))
    }

    /// [`BandStats::compute_with`] on `threads` threads. On one, one pass makes the count, range,
    /// sums and whether every value is whole, and a second the histogram and distinct values. On
    /// several, the sums are made on this thread, in the band's order, so the mean and spread are
    /// the same to the bit; what doesn't depend on the order is counted on the others
    /// ([`on_runs`]), the range while this thread sums. The range keeps the first of equal values
    /// (−0 or 0) in the band's order either way.
    fn compute_on(data: &[f32], valid: impl Fn(usize, f32) -> bool + Sync, stored: Option<pam::Stats>, threads: usize) -> BandStats {
        // A run of `data` from `start`.
        let ranged = |start: usize, run: &[f32], sum: bool| {
            let (mut n, mut min, mut max, mut whole, mut sums) = (0usize, f64::INFINITY, f64::NEG_INFINITY, true, (0.0f64, 0.0f64));
            for (j, &v) in run.iter().enumerate() {
                if !valid(start + j, v) {
                    continue;
                }
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
            ranged(0, data, true)
        } else {
            let sums = || ranged(0, data, true).4;
            let (runs, sums) = on_runs_beside(data, threads, sums, |start, run| ranged(start, run, false));
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
        let mut runs = on_runs(data, threads, |start, run| Tally::run(start, run, &valid, b)).into_iter();
        let mut tally = runs.next().unwrap_or_else(|| Tally::run(0, &[], &valid, b));
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

/// `f` over `threads` runs of `data` that cover it in order, each given with where it starts in
/// `data`, its results in that order, the calling thread taking the first. A worker's panic is
/// raised again here; a run whose thread can't start is made on this one.
fn on_runs<T: Send>(data: &[f32], threads: usize, f: impl Fn(usize, &[f32]) -> T + Sync) -> Vec<T> {
    on_runs_beside(data, threads, || (), f).0
}

/// [`on_runs`], the calling thread running `beside` first, while the other threads start on
/// their runs.
fn on_runs_beside<T: Send, U>(
    data: &[f32],
    threads: usize,
    beside: impl FnOnce() -> U,
    f: impl Fn(usize, &[f32]) -> T + Sync,
) -> (Vec<T>, U) {
    if threads <= 1 || data.is_empty() {
        let beside = beside();
        return (vec![f(0, data)], beside);
    }
    let size = data.len().div_ceil(threads);
    let mut runs = data.chunks(size).enumerate().map(|(k, run)| (k * size, run));
    let (start, first) = runs.next().unwrap_or_default();
    let f = &f;
    std::thread::scope(|scope| {
        let started: Vec<_> = runs
            .map(|(start, run)| std::thread::Builder::new().spawn_scoped(scope, move || f(start, run)).map_err(|_| (start, run)))
            .collect();
        let beside = beside();
        let mut out = vec![f(start, first)];
        for run in started {
            out.push(match run {
                Ok(h) => h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)),
                // The OS refused a thread: made here.
                Err((start, run)) => f(start, run),
            });
        }
        (out, beside)
    })
}

impl Tally {
    /// `data`'s valid values counted on this thread: a run of a band from `start`.
    fn run(start: usize, data: &[f32], valid: &impl Fn(usize, f32) -> bool, b: Binning) -> Tally {
        let mut histogram = vec![0u64; HISTOGRAM_BINS];
        let mut bits = if b.small_ints { vec![0u64; 1024] } else { Vec::new() };
        let mut distinct = std::collections::BTreeSet::new();
        let mut track = b.whole;
        for (j, &v) in data.iter().enumerate() {
            if !valid(start + j, v) {
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

/// Whether an alpha value draws its cell fully see-through, as the map draws it: an alpha of no
/// value (`valid` false: NaN, or the raster's no-data), or one under half a 256th of `opaque` (the
/// pixel type's [`alpha_max`]), which is nothing in 8 bits.
pub fn see_through(a: f32, valid: bool, opaque: f32) -> bool {
    !valid || ((a / opaque).clamp(0.0, 1.0) * 255.0).round() == 0.0
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

impl ColorHint {
    /// For a raster of `bands` bands shown this way with alpha, its alpha band and the colour
    /// bands that band makes see-through: an RGBA image's fourth band over its first three, grey
    /// and alpha's second over its first.
    pub fn alpha(&self, bands: usize) -> Option<(usize, std::ops::Range<usize>)> {
        match self {
            ColorHint::Rgb { alpha: true } if bands >= 4 => Some((3, 0..3)),
            ColorHint::GrayAlpha if bands >= 2 => Some((1, 0..1)),
            _ => None,
        }
    }
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
    /// The original local GeoTIFF's revision or capture failure, retained with these pixels and never serialized.
    #[cfg(not(target_arch = "wasm32"))]
    source_file: Option<std::result::Result<file_revision::FileRevision, String>>,
    /// Whether the pixels come from a reduced-resolution overview.
    pub overview: Option<(u32, u32)>,
    /// With `overview`, whether the pixels are every `step`-th of the file's, from its top-left
    /// corner (a picture too large to read whole): each then spans exactly `step` of its pixels,
    /// where an overview's span its size over the file's. 1 otherwise.
    pub(crate) step: u32,
    /// With `step`, whether each pixel read is centered on the file pixel it was read from (a
    /// grid of posts, SRTM and DTED: [`post_transform`]) rather than starting at it, as a
    /// picture's does.
    pub(crate) posts: bool,
    /// `false` for a scan (a TIFF with no GeoTIFF tags, a PNG, a JPEG): `transform` is then only
    /// a stand-in until the picture is placed or georeferenced, and says nothing about the ground.
    pub georeferenced: bool,
    /// Whether `crs` is WGS 84 assumed for a file that doesn't say its system, its coordinates
    /// looking like degrees (an ASCII grid without a `.prj`, a GeoTIFF without one), or says only
    /// that it is in degrees (a GeoTIFF's geographic model with no EPSG code, its datum unread):
    /// it names no system to give (a project taking its system from the first layer added).
    pub crs_assumed: bool,
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
        let data_type = data_type.into();
        let (alpha, opaque) = (color.alpha(bands.len()), alpha_max(&data_type));
        let stats: Vec<BandStats> = bands
            .iter()
            .enumerate()
            .map(|(i, data)| {
                let alpha = alpha.as_ref().filter(|(_, colour)| colour.contains(&i)).and_then(|(a, _)| bands.get(*a));
                if measure { BandStats::of_band(data, valid, alpha.map(Vec::as_slice), opaque, None) } else { BandStats::default() }
            })
            .collect();
        let bands = bands
            .into_iter()
            .zip(stats)
            .enumerate()
            .map(|(i, (data, stats))| Band { name: format!("Band {}", i + 1), stats, data, classes: Vec::new(), wavelength: None })
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
            data_type,
            source: None,
            #[cfg(not(target_arch = "wasm32"))]
            source_file: None,
            overview: None,
            step: 1,
            posts: false,
            georeferenced: true,
            crs_assumed: false,
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
        // Bands whose attribute table's class names aren't used: the file names their classes.
        let mut unused_tables = 0usize;
        // Worked out before the bands change, as a colour band's take its alpha band's values.
        let (alpha, opaque) = (self.color.alpha(self.bands.len()), alpha_max(&self.data_type));
        let mut measured: Vec<Option<BandStats>> = (0..self.bands.len())
            .map(|i| {
                let stored = pam.band(i + 1).and_then(|p| p.stats).filter(|_| overview);
                let data = self.bands.get(i).filter(|_| recompute || stored.is_some())?;
                let alpha = alpha.as_ref().filter(|(_, colour)| colour.contains(&i)).and_then(|(a, _)| self.bands.get(*a));
                Some(BandStats::of_band(&data.data, valid, alpha.map(|a| a.data.as_slice()), opaque, stored))
            })
            .collect();
        for (i, band) in self.bands.iter_mut().enumerate() {
            let p = pam.band(i + 1);
            if let Some(d) = p.and_then(|p| p.description.as_ref()) {
                band.name = d.clone();
            }
            // A table's names don't replace those the file's own metadata gave.
            if let Some(p) = p.filter(|p| !p.categories.is_empty()) {
                if p.from_table && !band.classes.is_empty() {
                    unused_tables += 1;
                } else {
                    band.classes = p.categories.clone();
                }
            }
            if let Some(w) = p.and_then(|p| p.wavelength) {
                band.wavelength = Some(w);
            }
            if let Some(stats) = measured.get_mut(i).and_then(Option::take) {
                band.stats = stats;
            }
        }
        if unused_tables > 0 {
            self.lost.push(format!(
                "{from}: {unused_tables} attribute table(s) of class names not read: the file's own metadata names those bands' classes"
            ));
        }
    }

    /// Longitude/latitude bounds, from points along the grid's edges.
    fn compute_extent(&self) -> Extent {
        extent_of(&self.transform, &self.transformer, self.width, self.height)
    }

    /// Refuses a raster with no coordinates, for the tools that would give their result a place
    /// on the ground (the result of georeferencing it first is another matter).
    pub fn require_coordinates(&self) -> std::result::Result<(), String> {
        if self.georeferenced {
            Ok(())
        } else {
            Err(format!("{} has no coordinates: georeference it first (raster.georeference, with the Imagery extension on)", self.name))
        }
    }

    /// This raster as a scan: its pixels with no coordinates, as `"as": "scan"` opens a picture.
    pub fn into_scan(mut self) -> Result<Raster> {
        if self.georeferenced {
            // Its pixels no longer follow the file's own grid, so no grid read may pair with them.
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.source_file = None;
            }
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

    /// How many of the file's pixels one pixel read spans, across and down: exactly `step` for
    /// every `step`-th pixel, an overview's size over the file's, else 1. (`overview` stays the
    /// file's own size, which `step` needn't divide, so a reader at a step goes by `step`.)
    pub fn file_pixels(&self) -> (f64, f64) {
        if self.step > 1 {
            return (f64::from(self.step), f64::from(self.step));
        }
        let (full_w, full_h) = self.overview.unwrap_or((self.width, self.height));
        (f64::from(full_w) / f64::from(self.width.max(1)), f64::from(full_h) / f64::from(self.height.max(1)))
    }

    /// Where this raster's top-left corner lies in the file's pixels: the origin of
    /// [`Raster::file_pixels`]. A grid of posts read at a step centers each cell on its post, so
    /// the corner is `(step - 1) / 2` file pixels before the first post's own; otherwise 0.
    pub fn file_origin(&self) -> (f64, f64) {
        if self.posts && self.step > 1 {
            let o = -(f64::from(self.step) - 1.0) / 2.0;
            return (o, o);
        }
        (0.0, 0.0)
    }

    /// For other crates' tests: [`Raster::read_at_step`] of a grid of posts, as the SRTM and
    /// DTED readers mark one.
    #[doc(hidden)]
    pub fn posts_read_at_step(self, step: u32, full: (u32, u32)) -> Raster {
        let mut r = self.read_at_step(step, full);
        r.posts = true;
        r
    }

    /// For other crates' tests: this raster as if read at every `step`-th pixel of a file of
    /// `full` pixels, as the readers mark one read within the memory budget.
    #[doc(hidden)]
    pub fn read_at_step(mut self, step: u32, full: (u32, u32)) -> Raster {
        self.step = step.max(1);
        self.overview = Some(full);
        self
    }

    /// A scan placed by its world file, in `crs`: georeferenced. The world file speaks of the
    /// file's pixels, so a scan read at a reduced size keeps its place with pixels as much
    /// larger. A rotated, skewed or mirrored one is resampled north up, as a rotated GeoTIFF is,
    /// keeping its bands' names, classes and wavelengths. When it can't be placed (it would lie
    /// nowhere it can be drawn, or be too large turned), the scan comes back unchanged with why.
    pub fn placed_by_world_file(mut self, wf: &world::WorldFile, crs: Crs) -> std::result::Result<Raster, (Box<Raster>, String)> {
        if self.georeferenced {
            return Err((Box::new(self), "only a raster with no coordinates can be placed".into()));
        }
        let (sx, sy) = self.file_pixels();
        let (affine, transformer) = match world_placement(wf, crs, self.width, self.height, (sx, sy)) {
            Ok(placed) => placed,
            Err(why) => return Err((Box::new(self), why)),
        };
        let mut r = if wf.is_north_up() {
            self.transform = GeoTransform { origin_x: affine[0], origin_y: affine[3], pixel_width: affine[1], pixel_height: affine[5] };
            self.crs = crs;
            self.transformer = transformer;
            self.extent = self.compute_extent();
            self
        } else {
            // The bands are only read, so a picture that can't be turned comes back as it was.
            let data: Vec<&[f32]> = self.bands.iter().map(|b| b.data.as_slice()).collect();
            let (data, ow, oh, t) = match north_up(&data, self.width, self.height, affine, max_samples()) {
                Ok(turned) => turned,
                Err(e) => return Err((Box::new(self), e.to_string())),
            };
            let mut r = match Raster::new(self.name.clone(), ow, oh, data, t, crs, self.nodata, self.color.clone(), self.data_type.clone())
            {
                Ok(r) => r,
                Err(e) => return Err((Box::new(self), e.to_string())),
            };
            // Each band keeps all it was but its pixels and their statistics.
            for (new, mut old) in r.bands.iter_mut().zip(std::mem::take(&mut self.bands)) {
                old.data = std::mem::take(&mut new.data);
                old.stats = new.stats.clone();
                *new = old;
            }
            // A picture read at a reduced size still says so, as a turned GeoTIFF does, with the
            // size the whole file has turned at its own resolution (its corners turned, over a
            // cell of its pixels' area, as `north_up` sizes a grid).
            let [_, xc, xr, _, yc, yr] = wf.affine();
            let file_cell = (xc * yr - xr * yc).abs().sqrt();
            r.overview = self.overview.map(|(fw, fh)| turned_size([xc, xr, yc, yr], fw, fh));
            // Each turned cell is exactly `step` of the file's pixels across, unless the turned
            // grid had to be made coarser to fit; then the sizes' ratio is the scale.
            let exact = (r.transform.pixel_width.abs() / (f64::from(self.step) * file_cell) - 1.0).abs() < 1e-9;
            r.step = if exact { self.step } else { 1 };
            (r.source, r.lost, r.copyright) = (self.source, self.lost, self.copyright);
            (r.rounded, r.rounded_some) = (self.rounded, self.rounded_some);
            r
        };
        r.georeferenced = true;
        Ok(r)
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

    /// The cells of `band` drawn see-through, by their place (row by row): `band` is a colour
    /// band of a raster with alpha ([`ColorHint::alpha`]) and the cell's alpha draws it so
    /// ([`see_through`]). Its statistics leave them out. `None` when the band has no such cells
    /// to have: no alpha, or the alpha band or a band of data beside the colours.
    pub fn see_through_cells(&self, band: usize) -> Option<impl Fn(usize) -> bool + Sync + '_> {
        let (a, colour) = self.color.alpha(self.bands.len())?;
        let alpha = &self.bands.get(a).filter(|_| colour.contains(&band))?.data;
        let opaque = alpha_max(&self.data_type);
        Some(move |i: usize| alpha.get(i).is_none_or(|&v| see_through(v, self.is_valid(v), opaque)))
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
        self.pixel_at_world(self.transformer.from_lonlat(lonlat)?)
    }

    /// The pixel under a place in the raster's own system.
    pub fn pixel_at_world(&self, w: Coord) -> Option<(i64, i64)> {
        let (c, r) = self.transform.to_pixel(w);
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

/// Reads a raster file by path: a picture with a world file beside it placed by it (#2075).
pub fn read_path(path: &Path) -> Result<Raster> {
    read_path_placing(path, true)
}

/// [`read_path_with`] without the world file beside a picture, or a PDF page's own coordinates:
/// the picture as the file holds it,
/// for one opened as a scan (`"as": "scan"`, or a layer laid over a placement reopened), so a
/// world file that is there, or turns up later, doesn't place or resample it. A GDAL file's
/// `subdataset` and `crs`, a Zarr store's variable, are read as [`read_path_with`] reads them.
pub fn read_path_unplaced(path: &Path, subdataset: Option<&str>, crs: Option<&str>) -> Result<Raster> {
    read_path_with_placing(path, subdataset, crs, false)
}

/// [`read_path`], applying a picture's world file when `world` is true.
fn read_path_placing(path: &Path, world: bool) -> Result<Raster> {
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
    // A PDF's first page, placed by its own coordinates when it has them (never a world file),
    // unless it is read as a scan.
    if is_pdf_name(&path.to_string_lossy()) {
        return read_pdf_page(path, None, world);
    }
    // The `.aux.xml` sidecar, read first: a no-data value it gives (for a file that has none) keeps
    // those pixels out of the rounded count as they are read.
    let pam = pam::read(path);
    let sidecar_nodata = pam.as_ref().and_then(pam::Pam::nodata);
    let mut r = match ext.as_str() {
        "tif" | "tiff" => {
            #[cfg(not(target_arch = "wasm32"))]
            let r = geotiff::read_file_with_nodata(path, &name, sidecar_nodata)?;
            #[cfg(target_arch = "wasm32")]
            let r = geotiff::read_with_nodata(std::io::BufReader::new(crate::provenance::open(path)?), &name, sidecar_nodata)?;
            r
        }
        "png" | "jpg" | "jpeg" => read_scan(&name, std::io::BufReader::new(crate::provenance::open(path)?))?,
        "asc" => {
            if meta.len() > MAX_ASCII_BYTES {
                return Err(DataError::Format("the ASCII grid is too large to open".into()));
            }
            let text = crate::provenance::read_to_string(path)?;
            read_ascii(path, &text, &name, sidecar_nodata)?
        }
        // Read message by message, never whole.
        "grb" | "grib" | "grb2" | "grib2" => {
            grib::read_with_nodata(std::io::BufReader::new(crate::provenance::open(path)?), &name, sidecar_nodata)?
        }
        // Elevation, streamed a row or record at a time once its name and size are checked, so the
        // file's bytes are never held whole: an SRTM tile's size says its grid, and a DTED file is
        // no larger than its largest cell when checked (bytes past its last column are counted).
        e if ELEVATION_EXTENSIONS.contains(&e) => {
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let source = std::io::BufReader::new(crate::provenance::open(path)?);
            if ext == "hgt" {
                let checked = hgt::check(file_name, meta.len())?;
                hgt::read_checked_from(file_name, checked, source)?
            } else {
                dted::read_file(file_name, meta.len(), source)?
            }
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
    // leaves out just its pixels. Only an ASCII grid, a GRIB file, or a GeoTIFF of a type `f32` can
    // round, is read again.
    let applied = pam.as_ref().map(|p| p.nodata_within(r.bands.len()));
    if let Some(nd) = applied
        && nd != sidecar_nodata
        && r.nodata.is_none()
        && (matches!(ext.as_str(), "asc" | "grb" | "grib" | "grb2" | "grib2")
            || (matches!(ext.as_str(), "tif" | "tiff")
                && matches!(r.data_type.as_str(), "UInt32" | "Int32" | "UInt64" | "Int64" | "Float64")))
    {
        // The second read gives all the first did: the first goes before it, so two aren't held.
        drop(r);
        r = if ext == "asc" {
            read_ascii(path, &crate::provenance::read_to_string(path)?, &name, nd)?
        } else if grib::EXTENSIONS.contains(&ext.as_str()) {
            grib::read_with_nodata(std::io::BufReader::new(crate::provenance::open(path)?), &name, nd)?
        } else {
            #[cfg(not(target_arch = "wasm32"))]
            let again = geotiff::read_file_with_nodata(path, &name, nd)?;
            #[cfg(target_arch = "wasm32")]
            let again = geotiff::read_with_nodata(std::io::BufReader::new(crate::provenance::open(path)?), &name, nd)?;
            again
        };
    }
    if let Some(p) = &pam {
        r.apply_pam(p, pam::SIDECAR);
    }
    if world && matches!(ext.as_str(), "tif" | "tiff" | "png" | "jpg" | "jpeg") {
        r = apply_world_file(path, r);
    }
    r.source = Some(path.display().to_string());
    Ok(r)
}

/// A picture at `path` with the world file beside it applied (#2075). A scan is placed by it, in
/// the coordinate system of the `.prj` beside it. Whatever goes wrong (no `.prj`, one that can't
/// be used, a world file that can't be read, or one that places it nowhere it can be drawn), the
/// picture still opens as the scan it was, and a notice says why. A GeoTIFF keeps its own tags,
/// and says so when the world file places it differently.
pub fn apply_world_file(path: &Path, mut r: Raster) -> Raster {
    let Some((found, read)) = world::beside(path) else { return r };
    let name = found.file_name().map_or_else(|| found.display().to_string(), |n| n.to_string_lossy().into_owned());
    let unplaced = "it opens without coordinates";
    let wf = match read {
        Ok(wf) => wf,
        Err(e) => {
            let then = if r.georeferenced { "the GeoTIFF's own coordinates are used" } else { unplaced };
            r.lost.push(format!("the world file {name} can't be read ({e}): {then}"));
            return r;
        }
    };
    if r.georeferenced {
        if !world_file_agrees(&r, &wf) {
            r.lost.push(format!("the world file {name} places it differently from the GeoTIFF's own tags, which are used"));
        }
        return r;
    }
    let (crs, note) = match sidecar_crs(path) {
        Ok(found) => found,
        Err(e) => {
            r.lost.push(format!("the world file {name} isn't used ({e}): {unplaced}"));
            return r;
        }
    };
    let Some(crs) = crs else {
        let why = note.map_or_else(String::new, |n| format!(" (the .prj file: {n})"));
        r.lost.push(format!(
            "the world file {name} places it, but no .prj beside it says the coordinate system{why}: {unplaced}; put a .prj next to it and add it again"
        ));
        return r;
    };
    match r.placed_by_world_file(&wf, crs) {
        Ok(mut r) => {
            r.lost.extend(note.map(|n| format!("the .prj file: {n}")));
            r
        }
        Err((r, why)) => {
            let mut r = *r;
            r.lost.push(format!("the world file {name} isn't used ({why}): {unplaced}"));
            r
        }
    }
}

/// Whether a world file places a GeoTIFF where its tags do: the same corner and pixel size, to a
/// thousandth of a pixel (at the file's resolution). A turned world file isn't compared: a turned
/// GeoTIFF is resampled north up as it is read, so its tags can't be compared with one, and
/// nothing is claimed.
fn world_file_agrees(r: &Raster, wf: &world::WorldFile) -> bool {
    if !wf.is_north_up() {
        return true;
    }
    let (sx, sy) = r.file_pixels();
    let t = r.transform;
    let (pw, ph) = (t.pixel_width / sx, t.pixel_height / sy);
    let [x0, xc, _, y0, _, yr] = wf.affine();
    let close = |a: f64, b: f64, unit: f64| (a - b).abs() <= unit.abs() * 1e-3;
    close(xc, pw, pw) && close(yr, ph, ph) && close(x0, t.origin_x, pw) && close(y0, t.origin_y, ph)
}

/// Reads a raster file, with the GDAL choices a raster GDAL reads may have been added with: its
/// subdataset (a NetCDF variable) and the coordinate system it doesn't say. Other files ignore
/// them.
pub fn read_path_with(path: &Path, subdataset: Option<&str>, crs: Option<&str>) -> Result<Raster> {
    read_path_with_placing(path, subdataset, crs, true)
}

/// [`read_path_with`], placing a picture by its world file, and a PDF page by its own coordinates,
/// when `world` (a scan's reads pass `false`): a Zarr store's variable, a PDF's page or a GDAL
/// file's subdataset by `subdataset`, else the file as [`read_path_placing`] reads it.
fn read_path_with_placing(path: &Path, subdataset: Option<&str>, crs: Option<&str>, world: bool) -> Result<Raster> {
    // A Zarr store's variable and slice (`t2m[time=3]`).
    #[cfg(not(target_arch = "wasm32"))]
    if zarr::is_store(path) {
        return zarr::read_dir(path, subdataset.unwrap_or(""), crs);
    }
    // A PDF's page, by its number.
    if subdataset.is_some() && is_pdf_name(&path.to_string_lossy()) {
        return read_pdf_page(path, subdataset, world);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if (subdataset.is_some() || crs.is_some()) && gdal::is_gdal_raster_name(&path.to_string_lossy()) {
        return read_converted(path, &gdal::convert(path, subdataset, crs)?);
    }
    let ignored = elevation_ignores(&path.to_string_lossy(), subdataset, crs);
    let mut r = read_path_placing(path, world)?;
    r.lost.extend(ignored);
    Ok(r)
}

/// What reading an SRTM tile or DTED file (`name`) with a subdataset (`"layer"`) or a coordinate
/// system (`"crs"`) says: they hold one band and give their own coordinates, so neither applies,
/// and the file is read without them. A command may give them, and so may a project saved when
/// these files went through GDAL. `None` for other files, or when neither is given.
pub fn elevation_ignores(name: &str, subdataset: Option<&str>, crs: Option<&str>) -> Option<String> {
    if !is_elevation_name(name) {
        return None;
    }
    // A DTED file naming a datum other than WGS 84 says so as it is read (`dted::read`).
    const PLACE: &str = "give their own coordinates, read as WGS 84 longitude/latitude (a DTED file naming another datum says so)";
    const BAND: &str = "hold one band of heights, with no layers to choose from";
    let given = |v: Option<&str>| v.is_some_and(|v| !v.trim().is_empty());
    Some(match (given(subdataset), given(crs)) {
        (true, true) => format!("the \"layer\" and \"crs\" given are ignored: SRTM and DTED files {BAND}, and {PLACE}"),
        (true, false) => format!("the \"layer\" given is ignored: SRTM and DTED files {BAND}"),
        (false, true) => format!("the \"crs\" given is ignored: SRTM and DTED files {PLACE}"),
        (false, false) => return None,
    })
}

/// Reads the GeoTIFF GDAL converted `path` to, named and sourced as `path` (and its subdataset).
#[cfg(not(target_arch = "wasm32"))]
pub fn read_converted(path: &Path, c: &gdal::Converted) -> Result<Raster> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("raster");
    crate::provenance::path(path);
    let mut r = crate::provenance::without(|| geotiff::read_file(&c.path, stem))?;
    // This revision belongs to GDAL's cache copy, not the original source named below.
    r.source_file = None;
    if let Some(s) = &c.subdataset {
        r.name = format!("{stem} {}", gdal::short_name(s));
    }
    r.source = Some(path.display().to_string());
    // What converting found that the GeoTIFF it made doesn't say (that file names the system GDAL
    // assumed as if it had been the file's own): said with the raster's other losses, whichever way
    // it is read, as a reload says them again.
    if c.assumed_lonlat {
        r.crs_assumed = true;
        r.lost.push(format!("{} (give \"crs\" if not)", assumed_wgs84_note("it doesn't say its coordinate system")));
    }
    if !c.others.is_empty() {
        let shown: Vec<&str> = c.others.iter().take(20).map(String::as_str).collect();
        let more = c.others.len().saturating_sub(shown.len());
        let tail = if more > 0 { format!(", and {more} more") } else { String::new() };
        r.lost.push(format!("it has {} more variables ({}{tail}): add one with \"layer\"", c.others.len(), shown.join(", ")));
    }
    Ok(r)
}

/// What a reader says when a grid whose file gives no usable coordinate system is read as WGS 84
/// longitude/latitude, because its coordinates fit in degrees (AGENTS.md, Never silent): `why` is
/// what the file lacks or gets wrong. One line a raster, whatever its size.
pub(crate) fn assumed_wgs84_note(why: &str) -> String {
    format!("{why}; its coordinates look like longitude/latitude, so it's read as WGS 84")
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
    // The step a picture is read at to fit the budget, shared by its channels: a × ch > max
    // exactly when a > ⌊max / ch⌋.
    let k = step_within(w as usize, h as usize, max / ch.max(1)).and_then(|k| u32::try_from(k).ok()).ok_or_else(too_large)?;
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
    r.step = k;
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

/// The size, in cells of a pixel's area, of the north-up box around `w`×`h` pixels placed by
/// `[x per column, x per row, y per column, y per row]`, as [`north_up`] sizes its grid.
fn turned_size([xc, xr, yc, yr]: [f64; 4], w: u32, h: u32) -> (u32, u32) {
    let cell = (xc * yr - xr * yc).abs().sqrt();
    let (fw, fh) = (f64::from(w), f64::from(h));
    let span = |a: f64, b: f64| (0.0f64).max(a).max(b).max(a + b) - (0.0f64).min(a).min(b).min(a + b);
    // A thousandth of a cell of slack, so a size that is whole on paper isn't rounded up by noise.
    let n = |len: f64| ((len / cell) - 1e-3).ceil().clamp(1.0, f64::from(u32::MAX)) as u32;
    if !cell.is_normal() {
        return (w.max(1), h.max(1));
    }
    (n(span(fw * xc, fh * xr)), n(span(fw * yc, fh * yr)))
}

/// A `w`×`h` image placed by a rotated or skewed `affine` (GDAL's order: x, x per column, x per
/// row, y, y per column, y per row), resampled (nearest pixel) to the north-up grid of square
/// pixels over its bounding box: pixels of the same area, coarser if that would hold more than
/// `max` samples. Outside the image is NaN (transparent). Returns the bands, their size and grid.
fn north_up<B: AsRef<[f32]>>(bands: &[B], w: u32, h: u32, affine: [f64; 6], max: usize) -> Result<(Vec<Vec<f32>>, u32, u32, GeoTransform)> {
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
            for (o, b) in out.iter_mut().zip(bands) {
                if let (Some(o), Some(v)) = (o.get_mut(dst), b.as_ref().get(src)) {
                    *o = *v;
                }
            }
        }
    }
    Ok((out, ow, oh, GeoTransform { origin_x: min_x, origin_y: max_y, pixel_width: cell, pixel_height: -cell }))
}

/// Decodes a bounded image and reserves each output band fallibly before converting its pixels.
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
    let mut bands = (0..4).map(|_| grid::<f32>(width as usize, height as usize)).collect::<Result<Vec<_>>>()?;
    for px in img.pixels() {
        for (b, v) in bands.iter_mut().zip(px.0) {
            b.push(f32::from(v));
        }
    }
    let transform =
        GeoTransform { origin_x: w, origin_y: n, pixel_width: (e - w) / f64::from(width), pixel_height: -(n - s) / f64::from(height) };
    Raster::new(name, width, height, bands, transform, Crs::LonLat, None, ColorHint::Rgb { alpha: true }, "UInt8")
}

/// [`read_bytes`], and for a PDF its page `page` (its number; the first when `None`), placed by
/// its coordinates when `place` (a scan otherwise): an upload or download of a PDF, and a
/// project's copy of one reopened at the page it was added at.
pub fn read_bytes_page(file_name: &str, bytes: &[u8], page: Option<&str>, place: bool) -> Result<Raster> {
    if !is_pdf_name(file_name) {
        return read_bytes(file_name, bytes);
    }
    #[cfg(feature = "pdf")]
    {
        // Refused before the contents are copied.
        if bytes.len() as u64 > pdf::MAX_BYTES {
            return Err(DataError::Format(format!("the PDF is larger than {} MB, too large to open", pdf::MAX_BYTES >> 20)));
        }
        let name = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("raster");
        pdf::read_bytes(name, bytes.to_vec(), page, place)
    }
    #[cfg(not(feature = "pdf"))]
    {
        let _ = (page, place);
        Err(DataError::Unsupported(PDF_ELSEWHERE.into()))
    }
}

/// Reads an owned upload or embedded raster without copying PDF bytes for the decoder.
/// Other formats borrow the same buffer, as [`read_bytes_page`] does.
pub fn read_shared_page(file_name: &str, bytes: std::sync::Arc<Vec<u8>>, page: Option<&str>, place: bool) -> Result<Raster> {
    #[cfg(feature = "pdf")]
    if is_pdf_name(file_name) {
        let name = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("raster");
        return pdf::read_shared(name, bytes, page, place);
    }
    read_bytes_page(file_name, &bytes, page, place)
}

/// Reads a raster from a file's name and bytes (a browser upload); a PDF's first page.
pub fn read_bytes(file_name: &str, bytes: &[u8]) -> Result<Raster> {
    if is_pdf_name(file_name) {
        return read_bytes_page(file_name, bytes, None, true);
    }
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
        "grb" | "grib" | "grb2" | "grib2" => grib::read_bytes(bytes, &name),
        // An upload keeps its file name, which places an SRTM tile.
        "hgt" => hgt::read(file_name, bytes),
        e if ELEVATION_EXTENSIONS.contains(&e) => dted::read(file_name, bytes),
        other => Err(DataError::Unsupported(format!(".{other}"))),
    }
}

/// An ASCII grid at `path` (its `text`), in the coordinate system of the `.prj` next to it.
fn read_ascii(path: &Path, text: &str, name: &str, nodata: Option<f64>) -> Result<Raster> {
    let (crs, note) = sidecar_crs(path)?;
    let note = note.map(|message| format!("the .prj file: {message}"));
    // An unreadable definition explains the assumption itself; a missing definition says it is
    // missing. A readable system keeps its other notes, without repeating the planetary body.
    let mut r = ascii::read_with_nodata(text, name, crs, nodata, note.as_deref().filter(|_| crs.is_none()))?;
    if crs.is_some() {
        r.lost.extend(note);
    }
    Ok(r)
}

/// The coordinate system and WKT notice in the `.prj` next to an ASCII grid. An unreadable
/// definition returns no system and explains why, so the grid says it assumes longitude/latitude. Another body's system RustGIS
/// doesn't understand is an error instead, because the grid would land in the wrong place as
/// longitude/latitude.
fn sidecar_crs(path: &Path) -> Result<(Option<Crs>, Option<String>)> {
    let Some(prj) = read_prj(path) else {
        // One that is there but can't be read as text (not UTF-8, too large to be one) is said too.
        let there = prj_path(path).is_some();
        return Ok((None, there.then(|| "it can't be read as text".to_string())));
    };
    match wkt::read_wkt(&prj) {
        Ok((crs, note)) => {
            // The raster's body channel reports planetary interpretation once; datum and
            // projection notices from this sidecar must otherwise be preserved, and so must a
            // shift the file gives that isn't applied.
            let note = if crs.is_planetary() { wkt::file_crs_note(&prj, crs) } else { note };
            Ok((Some(crs), note))
        }
        Err(e) => match planetary::refused_wkt(&prj, &e) {
            Some(refused) => Err(DataError::Format(format!("the .prj file: {refused}"))),
            None => Ok((None, Some(e))),
        },
    }
}

/// Whether a world file lays a `w` × `h` picture somewhere on Earth in `crs`: its corners convert,
/// and in a geographic system lie within the poles and at longitudes wrapping explains (the
/// check [`Raster::placed_by_world_file`] makes, on the corners alone, without the pixels).
pub(crate) fn lands_on_earth(wf: &world::WorldFile, crs: Crs, w: u32, h: u32) -> bool {
    world_placement(wf, crs, w, h, (1.0, 1.0)).is_ok()
}

/// Where a world file lays a `w` × `h` picture whose pixels each span `(sx, sy)` of the file's, in
/// `crs`: the affine of those pixels' corners (GDAL's order) and the system's transformer, or why
/// it lays the picture nowhere it can be drawn. Checked on the box of its corners, before any
/// pixel moves: the grid over it must be valid with an extent, and in a geographic system its
/// corners within the poles and at longitudes wrapping explains, since the extent clamps
/// latitudes and wraps longitudes and can't show numbers in another system (UTM meters with a
/// longitude/latitude .prj).
fn world_placement(
    wf: &world::WorldFile,
    crs: Crs,
    w: u32,
    h: u32,
    (sx, sy): (f64, f64),
) -> std::result::Result<([f64; 6], Transformer), String> {
    let transformer = crs.transformer()?;
    let [x0, xc, xr, y0, yc, yr] = wf.affine();
    let affine = [x0, xc * sx, xr * sy, y0, yc * sx, yr * sy];
    let (fw, fh) = (f64::from(w), f64::from(h));
    let corners = [(0.0, 0.0), (fw, 0.0), (0.0, fh), (fw, fh)]
        .map(|(c, r)| (affine[0] + c * affine[1] + r * affine[2], affine[3] + c * affine[4] + r * affine[5]));
    let (min_x, max_x) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), (x, _)| (a.min(*x), b.max(*x)));
    let (min_y, max_y) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), (_, y)| (a.min(*y), b.max(*y)));
    let boxed = GeoTransform {
        origin_x: min_x,
        origin_y: max_y,
        pixel_width: (max_x - min_x) / fw.max(1.0),
        pixel_height: -(max_y - min_y) / fh.max(1.0),
    };
    let on_earth = !transformer.is_geographic() || (min_y >= -90.5 && max_y <= 90.5 && min_x >= -540.0 && max_x <= 540.0);
    if !boxed.is_valid() || !on_earth || extent_of(&boxed, &transformer, w, h).is_empty() {
        return Err(format!("it places the picture nowhere it can be drawn in {}", crs.label()));
    }
    Ok((affine, transformer))
}

/// The `.prj` next to a file, in lower or upper case (`.PRJ`, beside an `IMAGE.TFW`), for file
/// systems that tell them apart.
fn prj_path(path: &Path) -> Option<std::path::PathBuf> {
    ["prj", "PRJ"].into_iter().map(|e| path.with_extension(e)).find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file()))
}

/// The text of the `.prj` next to a file (`None` when there's none, or it's too large to be one).
pub fn read_prj(path: &Path) -> Option<String> {
    let prj = prj_path(path)?;
    let meta = std::fs::metadata(&prj).ok()?;
    if !meta.is_file() || meta.len() > 1 << 20 {
        return None;
    }
    crate::provenance::read_to_string(prj).ok()
}

/// Longitude/latitude bounds of a `width` × `height` grid placed by `transform` in `transformer`'s
/// system, from points along its edges and inside it ([`Raster::compute_extent`]): also for a
/// placement checked before any pixel is moved.
fn extent_of(transform: &GeoTransform, transformer: &Transformer, width: u32, height: u32) -> Extent {
    let mut e = Extent::EMPTY;
    let (w, h) = (width as f64, height as f64);
    let steps = 32;
    let mut add = |col: f64, row: f64| {
        if let Some(c) = transformer.to_lonlat_for_extent(transform.to_world(col, row)) {
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
    if !e.is_empty() && transformer.is_geographic() && w * transform.pixel_width.abs() >= 360.0 {
        (e.min_x, e.max_x) = (-180.0, 180.0);
    } else if !e.is_empty() && (e.min_x < -180.0 || e.max_x > 180.0) {
        let shift = 360.0 * ((e.min_x + 180.0) / 360.0).floor();
        let (lo, hi) = (e.min_x - shift, e.max_x - shift);
        (e.min_x, e.max_x) = if lo >= -180.0 && hi <= 180.0 { (lo, hi) } else { (-180.0, 180.0) };
    }
    // A whole world in Natural Earth, Winkel Tripel or Eckert IV reaches the antimeridian and the
    // poles at its outline's edge, which samples miss when the grid's edges are a little past
    // it (its corners are off the outline): whatever of them is in the grid is in its extent
    // (#1701).
    if let Some(centre) = transformer.world_central_meridian().filter(|_| !e.is_empty()) {
        let inside = |lon: f64, lat: f64| {
            transformer.from_lonlat(Coord::new(lon, lat)).is_some_and(|m| {
                let (col, row) = transform.to_pixel(m);
                (-1e-6..=w + 1e-6).contains(&col) && (-1e-6..=h + 1e-6).contains(&row)
            })
        };
        let along = |i: usize| -180.0 + 360.0 * i as f64 / steps as f64;
        if (0..=steps).any(|i| inside(180.0, along(i) / 2.0)) {
            e.max_x = 180.0;
        }
        if (0..=steps).any(|i| inside(-180.0, along(i) / 2.0)) {
            e.min_x = -180.0;
        }
        if (0..=steps).any(|i| inside(along(i), 90.0)) {
            e.max_y = 90.0;
        }
        if (0..=steps).any(|i| inside(along(i), -90.0)) {
            e.min_y = -90.0;
        }
        // Centred elsewhere than on 0°, the outline's edges are another meridian than the
        // antimeridian: the one opposite its centre, on either side of it.
        let edge = (centre + 360.0).rem_euclid(360.0) - 180.0;
        if edge.abs() < 180.0 - 1e-6 && (0..=steps).any(|i| [-1e-9, 1e-9].iter().any(|d| inside(edge + d, along(i) / 2.0))) {
            (e.min_x, e.max_x) = (e.min_x.min(edge), e.max_x.max(edge));
        }
    }
    e
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn elevation_files_are_raster_names() {
        for ext in ELEVATION_EXTENSIONS {
            assert!(READ_EXTENSIONS.contains(&ext), "{ext}");
            assert!(is_raster_name(&format!("/d/N37W122.{}", ext.to_uppercase())), "{ext}");
            assert!(is_elevation_name(&format!("x.{ext}")), "{ext}");
        }
        assert!(!is_elevation_name("x.tif") && !is_elevation_name("hgt") && !is_elevation_name(""));
    }

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
        let value = |v: f32| v.is_finite() && v != -9999.0;
        let valid = |_: usize, v: f32| value(v);
        for (name, data) in &bands {
            let want = one_pass(data, value);
            for threads in [1, 2, 7, 16] {
                let got = BandStats::compute_on(data, valid, None, threads);
                assert_eq!(bits(&got), bits(&want), "{name} on {threads}");
                assert_eq!(got.histogram, want.histogram, "{name} on {threads}");
                assert_eq!(got.categories, want.categories, "{name} on {threads}");
            }
        }
        // A validity that depends on the cell's place, as a see-through cell's does (#1810): each
        // run is given where it starts, so several threads leave out the same cells as one. A
        // cell's place is checked against its band too: every third one is left out, and so is
        // the band's last cell, which a run off by one would miss or count twice.
        for (name, data) in &bands {
            let by_place = |i: usize, v: f32| value(v) && !i.is_multiple_of(3) && i + 1 != data.len();
            let kept: Vec<f32> = data.iter().enumerate().map(|(i, &v)| if by_place(i, v) { v } else { f32::NAN }).collect();
            let want = one_pass(&kept, value);
            for threads in [1, 2, 7, 16] {
                let got = BandStats::compute_on(data, by_place, None, threads);
                assert_eq!(bits(&got), bits(&want), "{name} by place on {threads}");
                assert_eq!(got.histogram, want.histogram, "{name} by place on {threads}");
                assert_eq!(got.categories, want.categories, "{name} by place on {threads}");
            }
        }
        assert!(BandStats::compute_on(&bands[1].1, valid, None, 2).categories.is_none(), "257 small whole values");
        assert!(BandStats::compute_on(&bands[3].1, valid, None, 2).categories.is_none(), "400 in two runs of 200");
        // A band too small for threads, and one with no valid value.
        let small: Vec<f32> = (0..1000).map(|i| (i % 37) as f32).collect();
        assert_eq!(BandStats::from_values(&small), one_pass(&small, f32::is_finite));
        assert_eq!(BandStats::compute_on(&vec![f32::NAN; n], valid, None, 4), BandStats::default());
    }

    /// Cells a raster's alpha draws see-through are left out of its colour bands' statistics,
    /// whatever their values (#1810): −3.4e38 in a Float32 image, a stray byte in an 8-bit one. An
    /// opaque black cell counts, the alpha band counts every cell, and a raster without alpha (or
    /// with a fourth band of data) counts every valid cell, as before.
    #[test]
    fn see_through_cells_are_left_out_of_colour_statistics() {
        let gt = GeoTransform { origin_x: 500_000.0, origin_y: 4_800_000.0, pixel_width: 1.0, pixel_height: -1.0 };
        let utm = Crs::from_epsg(32611).unwrap();
        // 4 × 4: the bottom row see-through (holding `see`), the first cell opaque black, the rest 100.
        let colour = |see: f32| {
            (0..16)
                .map(|i| {
                    if i >= 12 {
                        see
                    } else if i == 0 {
                        0.0
                    } else {
                        100.0
                    }
                })
                .collect::<Vec<f32>>()
        };
        let alpha = |opaque: f32| (0..16).map(|i| if i >= 12 { 0.0 } else { opaque }).collect::<Vec<f32>>();
        let stats = |b: &Band| (b.stats.count, b.stats.min, b.stats.max);
        for (data_type, see, opaque) in [("Float32", -3.4e38_f32, 1.0), ("UInt8", 7.0, 255.0), ("UInt16", 7.0, 65535.0)] {
            let bands = vec![colour(see), colour(see), colour(see), alpha(opaque)];
            let r = Raster::new("rgba", 4, 4, bands, gt, utm, None, ColorHint::Rgb { alpha: true }, data_type).unwrap();
            for b in &r.bands[..3] {
                assert_eq!(stats(b), (12, 0.0, 100.0), "{data_type}");
                // Eleven cells of 100 and one of 0: the mean GDAL 3.9.3 gives an 8-bit RGBA band.
                assert!((b.stats.mean - 1100.0 / 12.0).abs() < 1e-9, "{data_type}: {}", b.stats.mean);
                assert_eq!(b.stats.histogram.iter().sum::<u64>(), 12, "{data_type}");
            }
            assert_eq!(stats(&r.bands[3]), (16, 0.0, f64::from(opaque)), "{data_type}");
            // Read back from its GeoTIFF, the same.
            let back = geotiff::read(std::io::Cursor::new(geotiff::write(&r, 0).unwrap()), "back").unwrap();
            assert_eq!(back.color, ColorHint::Rgb { alpha: true }, "{data_type}");
            // Written in the type it was before: a Float32 image whose see-through cells hold
            // −3.4e38 isn't taken for bytes, as a GeoTIFF or a cloud-optimised one.
            if data_type == "Float32" {
                let cog = geotiff::read(std::io::Cursor::new(cogwrite::write(&r, 0).unwrap()), "cog").unwrap();
                assert_eq!((back.data_type.as_str(), cog.data_type.as_str()), ("Float32", "Float32"));
            }
            assert_eq!(back.bands.iter().map(stats).collect::<Vec<_>>(), r.bands.iter().map(stats).collect::<Vec<_>>(), "{data_type}");
        }
        // As the map draws it: an alpha under half a 256th of opaque is see-through (16 bits: 100
        // of 65535), one above it isn't (200).
        let faint: Vec<f32> = (0..16)
            .map(|i| {
                if i >= 12 {
                    100.0
                } else if i >= 8 {
                    200.0
                } else {
                    65535.0
                }
            })
            .collect();
        let r = Raster::new(
            "faint",
            4,
            4,
            vec![colour(7.0), colour(7.0), colour(7.0), faint],
            gt,
            utm,
            None,
            ColorHint::Rgb { alpha: true },
            "UInt16",
        )
        .unwrap();
        assert_eq!(r.bands[0].stats.count, 12);
        // Grey and alpha: the grey band.
        let r = Raster::new("grey", 4, 4, vec![colour(7.0), alpha(255.0)], gt, utm, None, ColorHint::GrayAlpha, "UInt8").unwrap();
        assert_eq!((stats(&r.bands[0]), r.bands[1].stats.count), ((12, 0.0, 100.0), 16));
        // No-data 0: an alpha of no value is see-through, as the map draws it, and the black cell
        // is no-data, as before.
        let bands = vec![colour(-3.4e38), colour(-3.4e38), colour(-3.4e38), alpha(1.0)];
        let r = Raster::new("nodata", 4, 4, bands, gt, utm, Some(0.0), ColorHint::Rgb { alpha: true }, "Float32").unwrap();
        assert_eq!(stats(&r.bands[0]), (11, 100.0, 100.0));
        // Without alpha every valid cell counts: RGB, and RGB with a fourth band of data.
        for nb in [3, 4] {
            let bands = (0..nb).map(|_| colour(7.0)).collect();
            let r = Raster::new("rgb", 4, 4, bands, gt, utm, None, ColorHint::Rgb { alpha: false }, "UInt8").unwrap();
            assert!(r.bands.iter().all(|b| stats(b) == (16, 0.0, 100.0)), "{nb} bands");
        }
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
    fn gdal_sidecars_name_classes_from_an_attribute_table() {
        let dir = std::env::temp_dir().join(format!("rustgis-pam-rat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gt = GeoTransform { origin_x: 10.0, origin_y: 50.0, pixel_width: 0.5, pixel_height: -0.5 };
        let r = Raster::new("lc", 2, 2, vec![vec![1.0, 3.0, 3.0, 1.0]], gt, Crs::LonLat, None, ColorHint::Gray, "UInt8").unwrap();
        let tif = dir.join("lc.tif");
        std::fs::write(&tif, geotiff::write(&r, 0).unwrap()).unwrap();
        let xml = r#"<PAMDataset><PAMRasterBand band="1"><GDALRasterAttributeTable Row0Min="0" BinSize="1">
<FieldDefn index="0"><Name>Value</Name><Type>0</Type><Usage>5</Usage></FieldDefn>
<FieldDefn index="1"><Name>Class_Name</Name><Type>2</Type><Usage>2</Usage></FieldDefn>
<Row index="0"><F>1</F><F>Water</F></Row><Row index="1"><F>3</F><F>Forest</F></Row>
</GDALRasterAttributeTable></PAMRasterBand></PAMDataset>"#;
        std::fs::write(dir.join("lc.tif.aux.xml"), xml).unwrap();
        let back = read_path(&tif).unwrap();
        let b = &back.bands[0];
        assert_eq!(b.classes, ["", "Water", "", "Forest"]);
        assert_eq!((b.class_name(1.0), b.class_name(3.0), b.class_name(2.0)), (Some("Water"), Some("Forest"), None));
        assert!(back.lost.is_empty(), "{:?}", back.lost);
        // Class names the GeoTIFF gives itself are kept over the table's, and that is said.
        let mut named = r.clone();
        named.bands[0].classes = vec![String::new(), "Lake".into()];
        std::fs::write(&tif, geotiff::write(&named, 0).unwrap()).unwrap();
        let own = read_path(&tif).unwrap();
        assert_eq!(own.bands[0].classes, ["", "Lake"]);
        assert_eq!(
            own.lost,
            ["its .aux.xml sidecar: 1 attribute table(s) of class names not read: the file's own metadata names those bands' classes"]
        );
        std::fs::remove_dir_all(&dir).ok();
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

    /// A whole world in Natural Earth, Winkel Tripel or Eckert IV reaches both sides of the
    /// antimeridian and both poles, with its edges on the outline or a little past it, as a warp
    /// leaves them; its east half reaches 180° and not -180°, and Europe stays Europe (#1701).
    #[test]
    fn a_whole_world_in_a_world_projection_has_a_world_wide_extent() {
        for def in [
            "+proj=natearth +lon_0=0 +datum=WGS84 +units=m +no_defs",
            "+proj=wintri +lon_0=0 +lat_1=50.459776252189805 +datum=WGS84 +units=m +no_defs",
            "+proj=eck4 +lon_0=0 +datum=WGS84 +units=m +no_defs",
        ] {
            let crs = Crs::parse(def).unwrap();
            let t = crs.transformer().unwrap();
            let at = |lon: f64, lat: f64| t.from_lonlat(Coord::new(lon, lat)).unwrap();
            let (east, north) = (at(180.0, 0.0).x, at(0.0, 90.0).y);
            let raster = |(west, south, east, north): (f64, f64, f64, f64)| {
                let (w, h) = (64, 32);
                let gt = GeoTransform {
                    origin_x: west,
                    origin_y: north,
                    pixel_width: (east - west) / w as f64,
                    pixel_height: (south - north) / h as f64,
                };
                Raster::new("w", w, h, vec![vec![1.0; (w * h) as usize]], gt, crs, None, ColorHint::Gray, "Float32").unwrap()
            };
            for past in [1.0, 1.001, 1.01] {
                let e = raster((-east * past, -north * past, east * past, north * past)).extent();
                assert_eq!((e.min_x, e.max_x, e.min_y, e.max_y), (-180.0, 180.0, -90.0, 90.0), "{def} {past}: {e:?}");
                // The east half: 0° to 180°.
                let e = raster((0.0, -north * past, east * past, north * past)).extent();
                assert!(e.min_x.abs() < 1e-6 && e.max_x == 180.0, "{def} {past}: {e:?}");
            }
            let (sw, ne) = (at(-10.0, 35.0), at(30.0, 70.0));
            let e = raster((sw.x, sw.y, ne.x, ne.y)).extent();
            assert!(e.min_x > -40.0 && e.max_x < 60.0 && e.min_y > 30.0 && e.max_y < 75.0, "{def}: {e:?}");
            // Centred on 150°E, the outline's edges are 30°W: the west half is 30°W to 150°E, a
            // whole world all of it, and the east half, across the antimeridian, all of it too.
            let crs = Crs::parse(&def.replace("+lon_0=0", "+lon_0=150")).unwrap();
            let raster = |(west, south, east, north): (f64, f64, f64, f64)| {
                let (w, h) = (64, 32);
                let gt = GeoTransform {
                    origin_x: west,
                    origin_y: north,
                    pixel_width: (east - west) / w as f64,
                    pixel_height: (south - north) / h as f64,
                };
                Raster::new("w", w, h, vec![vec![1.0; (w * h) as usize]], gt, crs, None, ColorHint::Gray, "Float32").unwrap()
            };
            for past in [1.0, 1.001, 1.01] {
                let e = raster((-east * past, -north * past, 0.0, north * past)).extent();
                assert!((e.min_x + 30.0).abs() < 1e-6 && (e.max_x - 150.0).abs() < 1e-6, "{def} at 150°E, west half, {past}: {e:?}");
                assert_eq!((e.min_y, e.max_y), (-90.0, 90.0), "{def} at 150°E, west half, {past}: {e:?}");
                for west in [-east * past, 0.0] {
                    let e = raster((west, -north * past, east * past, north * past)).extent();
                    assert_eq!((e.min_x, e.max_x), (-180.0, 180.0), "{def} at 150°E from {west}, {past}: {e:?}");
                }
            }
        }
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

    /// An ASCII grid on disk says it is read as WGS 84 when no `.prj` gives its system, and says
    /// nothing when one does; a `.prj` that can't be read adds why, and the assumption is said once.
    #[test]
    fn an_ascii_grid_on_disk_says_what_its_prj_leaves_out() {
        let dir = std::env::temp_dir().join(format!("rustgis-ascii-assumed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let asc = dir.join("g.asc");
        std::fs::write(&asc, "ncols 2\nnrows 1\nxllcorner 10\nyllcorner 40\ncellsize 1\n1 2\n").unwrap();
        let r = read_path(&asc).unwrap();
        assert_eq!(r.crs, Crs::LonLat);
        assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
        assert!(r.lost[0].contains("doesn't say its coordinate system") && r.lost[0].contains("WGS 84"), "{:?}", r.lost);
        std::fs::write(dir.join("g.prj"), wkt::to_wkt(Crs::LonLat).unwrap()).unwrap();
        let r = read_path(&asc).unwrap();
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        std::fs::write(dir.join("g.prj"), "not a coordinate system").unwrap();
        let r = read_path(&asc).unwrap();
        assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
        let note = &r.lost[0];
        assert!(note.starts_with("the .prj file: ") && note.contains("so it's read as WGS 84"), "{note}");
        assert!(!note.contains("it doesn't say its coordinate system"), "{note}");
        std::fs::write(&asc, "ncols 2\nnrows 1\nxllcorner 1000\nyllcorner 4000\ncellsize 1\n1 2\n").unwrap();
        let error = read_path(&asc).unwrap_err().to_string();
        assert!(error.contains("the .prj file:") && error.contains("unknown coordinate system"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What GDAL's conversion found (a coordinate system it assumed, variables it didn't add) stays
    /// with the raster read from the converted file, whichever way it is read; a conversion that found
    /// nothing says nothing.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_converted_raster_keeps_what_converting_found() {
        let dir = std::env::temp_dir().join(format!("rustgis-converted-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gt = GeoTransform { origin_x: 10.0, origin_y: 41.0, pixel_width: 1.0, pixel_height: -1.0 };
        let made = Raster::new("g", 2, 1, vec![vec![1.0, 2.0]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let tif = dir.join("converted.tif");
        std::fs::write(&tif, geotiff::write(&made, 0).unwrap()).unwrap();
        let lossy = gdal::Converted {
            path: tif.clone(),
            subdataset: Some("temperature".into()),
            others: vec!["pressure".into(), "wind".into()],
            assumed_lonlat: true,
        };
        let r = read_converted(Path::new("/data/climate.nc"), &lossy).unwrap();
        assert_eq!(r.lost.len(), 2, "{:?}", r.lost);
        assert!(
            r.lost.iter().any(|n| n.contains("doesn't say its coordinate system") && n.contains("WGS 84") && n.contains("\"crs\"")),
            "{:?}",
            r.lost
        );
        assert!(r.lost.iter().any(|n| n.contains("pressure") && n.contains("wind") && n.contains("\"layer\"")), "{:?}", r.lost);
        // Past the 20 names shown, the count of the rest is said too.
        let many = gdal::Converted {
            path: tif.clone(),
            subdataset: None,
            others: (1..=35).map(|i| format!("band{i}")).collect(),
            assumed_lonlat: false,
        };
        let r = read_converted(Path::new("/data/climate.nc"), &many).unwrap();
        assert!(
            r.lost
                .iter()
                .any(|n| n.contains("35 more variables") && n.contains("band20") && !n.contains("band21") && n.contains("and 15 more")),
            "{:?}",
            r.lost
        );
        let clean = gdal::Converted { path: tif, subdataset: None, others: Vec::new(), assumed_lonlat: false };
        let r = read_converted(Path::new("/data/climate.nc"), &clean).unwrap();
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn annotation_grid_converted_pixels_do_not_claim_the_cache_revision() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("converted.tif");
        std::fs::write(&cache, geotiff::write(&small(), 0).unwrap()).unwrap();
        geotiff::read_file(&cache, "cache").unwrap().check_original_file(&cache).unwrap();
        let converted = gdal::Converted { path: cache.clone(), subdataset: None, others: Vec::new(), assumed_lonlat: false };
        let source = dir.path().join("original.nc");
        let pixels = read_converted(&source, &converted).unwrap();
        assert_eq!(pixels.source.as_deref(), source.to_str());
        let error = pixels.check_original_file(&cache).unwrap_err();
        assert!(error.contains("no verified original file revision"), "{error}");
        assert!(!error.contains("file changed"), "{error}");
    }
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_turned_size_is_the_grid_north_up_makes() {
        let (w, h) = (40u32, 17u32);
        let band = vec![1.0f32; (w * h) as usize];
        for degrees in [0.0f64, 30.0, 90.0, 135.0, 200.0, 271.0] {
            let (sin, cos) = degrees.to_radians().sin_cos();
            let (xc, xr, yc, yr) = (2.5 * cos, 2.5 * sin, 2.5 * sin, -2.5 * cos);
            let (_, ow, oh, _) = north_up(&[band.as_slice()], w, h, [100.0, xc, xr, 50.0, yc, yr], usize::MAX).unwrap();
            assert_eq!(turned_size([xc, xr, yc, yr], w, h), (ow, oh), "{degrees}°");
        }
    }

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
        let placed = from_image("o", &encode(picture(4, 4, true), image::ImageFormat::Png), [0.0, 0.0, 1.0, 1.0], 0.0).unwrap();
        assert!(placed.georeferenced);
        assert_eq!(placed.color, ColorHint::Rgb { alpha: true });
        for (band, expected) in placed.bands.iter().zip([3.0, 2.0, 7.0, 200.0]) {
            assert_eq!(band.data.len(), 16);
            assert_eq!(band.data.get(11).copied(), Some(expected));
        }
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
#[ignore = "needs external raster samples in RUSTGIS_SAMPLE_RASTERS"]
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
