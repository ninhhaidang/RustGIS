//! PDF pages read as rasters (#2336), on the desktop: a page drawn as a picture by `hayro`, placed
//! where its GeoPDF viewport says (ISO 32000-2 § 12.10: a `/VP` entry whose `/Measure` is of
//! subtype `/GEO`), or else a scan with no coordinates, to place with Georeference.
//!
//! The whole page is drawn, its title and legend too, and the map frame's coordinates run across
//! it, as readers of GeoPDFs do.

use std::path::Path;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::object::{Array, Dict, Name, String as PdfString};
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{Affine, Point};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};
use rustgis_geom::Coord;

use super::{ColorHint, Crs, Raster, crs, grid, max_samples, unreferenced, wkt, world};
use crate::{DataError, Result};

/// The resolution a page is drawn at, in dots an inch, unless that wouldn't fit in memory.
pub const DPI: f64 = 150.0;
/// The longest side a page is drawn at, in pixels (the renderer's limit).
const MAX_SIDE: f64 = 65_535.0;
/// The largest PDF file read: it is held in memory whole while its page is drawn.
pub const MAX_BYTES: u64 = 1 << 30;
/// The most other pages a notice names.
const NAMED_PAGES: usize = 20;

/// Reads page `page` of the PDF at `path` (its number, from 1; the first when `None`), placed by
/// its coordinates when `place` (else a scan, as `"as": "scan"` opens it).
pub fn read_file(path: &Path, page: Option<&str>, place: bool) -> Result<Raster> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(DataError::Format(format!("{} is not a regular file", path.display())));
    }
    if meta.len() > MAX_BYTES {
        return Err(DataError::Format(format!("the PDF is larger than {} MB, too large to open", MAX_BYTES >> 20)));
    }
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("raster");
    let mut r = read_bytes(name, crate::provenance::read(path)?, page, place)?;
    r.source = Some(path.display().to_string());
    Ok(r)
}

/// Reads page `page` (its number, from 1; the first when `None`) of a PDF's contents, named
/// `name`. When `place`, a page with a GeoPDF viewport is placed by it; otherwise, or without
/// one, it is a scan. Other pages, other viewports, and a page drawn at less than [`DPI`] are said
/// in `lost`.
///
/// Reading the file and drawing the page are each one step that can't be stopped: Cancel is looked
/// at before, between and after them where the run asks for it ([`crate::progress`], as the read
/// queue's does). A download's page the app draws on a thread of its own is given up by the app
/// instead, before its turn, or dropped once drawn.
pub fn read_bytes(name: &str, bytes: Vec<u8>, page: Option<&str>, place: bool) -> Result<Raster> {
    read_limited(name, bytes, page, place, max_samples())
}

/// [`read_bytes`], drawing the page in at most `max` samples (the tests' small limit exercises the
/// reduction).
fn read_limited(name: &str, bytes: Vec<u8>, page: Option<&str>, place: bool, max: usize) -> Result<Raster> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(DataError::Format(format!("the PDF is larger than {} MB, too large to open", MAX_BYTES >> 20)));
    }
    crate::progress::check()?;
    let pdf = Pdf::new(bytes).map_err(|e| {
        DataError::Format(match e {
            hayro::hayro_syntax::LoadPdfError::Decryption(_) => "the PDF is encrypted, and isn't read".to_string(),
            hayro::hayro_syntax::LoadPdfError::Invalid => "not a PDF RustGIS can read".to_string(),
        })
    })?;
    let pages = pdf.pages();
    let count = pages.len();
    if count == 0 {
        return Err(DataError::Format("the PDF has no pages".into()));
    }
    let number = match page.map(str::trim).filter(|p| !p.is_empty()) {
        None => 1,
        Some(p) => p
            .strip_prefix("page ")
            .unwrap_or(p)
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=count).contains(n))
            .ok_or_else(|| DataError::Format(format!("no page '{p}': give a number from 1 to {count}")))?,
    };
    let Some(page) = pages.get(number - 1) else {
        return Err(DataError::Format(format!("no page {number}")));
    };
    let title = if count > 1 { format!("{name} page {number}") } else { name.to_string() };
    crate::progress::check()?;
    let (mut r, scale) = draw(page, &title, max)?;
    crate::progress::check()?;
    if count > 1 {
        // Only the pages named are written out, however many the file has.
        let others = count - 1;
        let shown: Vec<String> = (1..=count).filter(|n| *n != number).take(NAMED_PAGES).map(|n| n.to_string()).collect();
        let more = others.saturating_sub(NAMED_PAGES);
        let tail = if more > 0 { format!(", and {more} more") } else { String::new() };
        r.lost.push(format!("it has {others} more pages ({}{tail}): add one with \"layer\"", shown.join(", ")));
    }
    Ok(if place { place_by_viewport(r, page, scale) } else { r })
}

/// The page drawn on white, as red, green and blue bands, and the pixels a point spans: at
/// [`DPI`], or less where its bands wouldn't fit in `max` samples or within the renderer's limit
/// (said in `lost`).
fn draw(page: &Page<'_>, name: &str, max: usize) -> Result<(Raster, f64)> {
    let (w, h) = page.render_dimensions();
    let (w, h) = (f64::from(w), f64::from(h));
    if !(w >= 1.0 && h >= 1.0 && w.is_finite() && h.is_finite()) {
        return Err(DataError::Format("the page has no area".into()));
    }
    let wanted = DPI / 72.0;
    // Three samples a pixel kept (red, green, blue); the drawing's four bytes a pixel are a
    // fraction of what they take as `f32`.
    let fits = (max as f64 / 3.0 / (w * h)).sqrt();
    let largest = MAX_SIDE / w.max(h);
    let scale = wanted.min(largest).min(fits);
    if w * scale < 1.0 || h * scale < 1.0 {
        return Err(DataError::Format("the page is too large to draw".into()));
    }
    let settings = PixmapSettings { x_scale: scale as f32, y_scale: scale as f32, bg_color: WHITE };
    let pixmap = hayro::render(page, &RenderCache::new(), &InterpreterSettings::default(), &RenderSettings::default(), &settings);
    let (pw, ph) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
    // On an opaque background every pixel is opaque, so its premultiplied colour is its colour:
    // red, green and blue, the alpha left out.
    let n = pw as usize * ph as usize;
    let mut bands: Vec<Vec<f32>> = (0..3).map(|_| grid(pw as usize, ph as usize)).collect::<Result<_>>()?;
    for px in pixmap.data_as_u8_slice().as_chunks::<4>().0.iter().take(n) {
        for (band, v) in bands.iter_mut().zip(px) {
            band.push(f32::from(*v));
        }
    }
    drop(pixmap);
    let mut r = Raster::new(name, pw, ph, bands, unreferenced(pw, ph), Crs::LonLat, None, ColorHint::Rgb { alpha: false }, "UInt8")?;
    r.georeferenced = false;
    if scale < wanted * 0.999 {
        let why = if fits <= largest { "to fit in memory" } else { "the largest size it can be drawn at" };
        r.lost.push(format!("its page is drawn at {:.0} dots an inch, not {DPI:.0}, {why}", scale * 72.0));
    }
    Ok((r, scale))
}

/// A GeoPDF viewport: its rectangle on the page, its control points (latitude, longitude) and
/// where they lie in that rectangle (fractions of it), and its coordinate system with the PROJ
/// definition whose datum the points are given on.
struct Viewport {
    bbox: [f64; 4],
    gpts: Vec<(f64, f64)>,
    lpts: Vec<(f64, f64)>,
    crs: Crs,
    def: Option<String>,
    note: Option<String>,
}

impl Viewport {
    fn area(&self) -> f64 {
        ((self.bbox[2] - self.bbox[0]) * (self.bbox[3] - self.bbox[1])).abs()
    }
}

/// The page's GeoPDF viewports, and why any that would place it can't be read.
fn viewports(page: &Page<'_>) -> (Vec<Viewport>, Unread) {
    let (mut found, mut unread) = (Vec::new(), Unread::default());
    let Some(vps) = page.raw().get::<Array>(b"VP".as_slice()) else { return (found, unread) };
    // An entry that isn't a viewport's dictionary is counted, not passed over.
    let dicts: Vec<Dict> = vps.iter::<Dict>().collect();
    let strays = vps.raw_iter().count().saturating_sub(dicts.len());
    if strays > 0 {
        unread.add("an entry of its viewports isn't one".into(), strays);
    }
    for vp in dicts {
        let Some(measure) = vp.get::<Dict>(b"Measure".as_slice()) else { continue };
        // A measure in plain units (a drawing's scale) says nothing of the ground.
        if measure.get::<Name>(b"Subtype".as_slice()).is_none_or(|n| n.as_str() != "GEO") {
            continue;
        }
        match viewport(&vp, &measure) {
            Ok(v) => found.push(v),
            Err(why) => unread.add(why, 1),
        }
    }
    (found, unread)
}

/// A page's map frames that can't be used: how many, and why the first can't.
#[derive(Default)]
struct Unread {
    count: usize,
    first: Option<String>,
}

impl Unread {
    /// Counts `n` more frames that can't be used for `why`.
    fn add(&mut self, why: String, n: usize) {
        self.count = self.count.saturating_add(n);
        self.first.get_or_insert(why);
    }
}

/// Pairs of numbers from an array, or why it isn't one: an entry that isn't a number, or an odd
/// count, would pair every later point with the wrong partner.
fn pairs(a: &Array, what: &str) -> std::result::Result<Vec<(f64, f64)>, String> {
    let n: Vec<f64> = a.iter::<f64>().collect();
    let (paired, rest) = n.as_chunks::<2>();
    if n.len() != a.raw_iter().count() || !rest.is_empty() {
        return Err(format!("its {what} aren't pairs of numbers"));
    }
    Ok(paired.iter().map(|&[x, y]| (x, y)).collect())
}

/// One GeoPDF viewport, or why it can't be used.
fn viewport(vp: &Dict<'_>, measure: &Dict<'_>) -> std::result::Result<Viewport, String> {
    // Every entry a number, so one that isn't can't shift the corners.
    let bbox = vp.get::<Array>(b"BBox".as_slice());
    let numbers: Vec<f64> = bbox.as_ref().map(|a| a.iter::<f64>().collect()).unwrap_or_default();
    let entries = bbox.as_ref().map_or(0, |a| a.raw_iter().count());
    let bbox: [f64; 4] = numbers.try_into().ok().filter(|_| entries == 4).ok_or_else(|| "its rectangle isn't four numbers".to_string())?;
    let gpts = measure.get::<Array>(b"GPTS".as_slice()).map(|a| pairs(&a, "control points (/GPTS)")).transpose()?.unwrap_or_default();
    // Where the points lie in the rectangle.
    let lpts = measure
        .get::<Array>(b"LPTS".as_slice())
        .ok_or_else(|| "it doesn't say where its control points lie in it (/LPTS)".to_string())
        .and_then(|a| pairs(&a, "control points' places (/LPTS)"))?;
    if gpts.len() < 3 || gpts.len() != lpts.len() {
        return Err("its control points are missing or don't pair up".into());
    }
    let all = bbox.iter().chain(gpts.iter().flat_map(|(a, b)| [a, b])).chain(lpts.iter().flat_map(|(a, b)| [a, b]));
    if !all.into_iter().all(|v| v.is_finite()) {
        return Err("a number in it isn't one".into());
    }
    let gcs = measure.get::<Dict>(b"GCS".as_slice()).ok_or("it doesn't say its coordinate system")?;
    let epsg = gcs.get::<i64>(b"EPSG".as_slice()).and_then(|c| u32::try_from(c).ok());
    let text = gcs.get::<PdfString>(b"WKT".as_slice()).map(|w| String::from_utf8_lossy(w.as_bytes()).into_owned());
    // As a `.prj` and a code are read together elsewhere (a GeoPackage's): the code, unless the
    // text gives another shift or body; the text alone when the code isn't one RustGIS knows.
    let unknown = |code: u32| format!("its coordinate system, EPSG:{code}, isn't one RustGIS knows");
    let unreadable = |e: String| format!("its coordinate system can't be read ({e})");
    let (crs, note) = match (text.as_deref(), epsg) {
        (Some(t), Some(code)) => match (wkt::read_wkt_over_code(t, code), Crs::from_epsg(code)) {
            (Some(read), _) => read.map_err(unreadable)?,
            (None, Some(crs)) => (crs, None),
            (None, None) => wkt::read_wkt(t).map_err(|_| unknown(code))?,
        },
        (Some(t), None) => wkt::read_wkt(t).map_err(unreadable)?,
        (None, Some(code)) => (Crs::from_epsg(code).ok_or_else(|| unknown(code))?, None),
        (None, None) => return Err("it doesn't say its coordinate system".into()),
    };
    let def = match crs {
        Crs::Epsg(c) => crs::epsg_proj4(c).map(str::to_string),
        Crs::Custom(_) => crs.custom_def().map(|d| d.proj4.clone()),
        _ => None,
    };
    Ok(Viewport { bbox, gpts, lpts, crs, def, note })
}

/// `r`, the drawn page at `scale` pixels a point, placed by the largest of its GeoPDF viewports:
/// left a scan when it has none, or none that can be used (said in `lost`).
fn place_by_viewport(mut r: Raster, page: &Page<'_>, scale: f64) -> Raster {
    let (mut found, unread) = viewports(page);
    let unplaced = "it opens without coordinates";
    if found.is_empty() {
        if let Some(why) = &unread.first {
            r.lost.push(format!("{} ({why}, the first): {unplaced}", unusable_frames(unread.count)));
        }
        return r;
    }
    // The largest first; one that can't be fitted or placed gives way to the next.
    found.sort_by(|a, b| b.area().total_cmp(&a.area()));
    let (count, mut failed) = (found.len(), Vec::new());
    let to_pixels = Affine::scale(scale) * page.initial_transform(true).to_kurbo();
    for v in found {
        let (wf, off) = match fit(&v, to_pixels, r.width, r.height) {
            Ok(fitted) => fitted,
            Err(why) => {
                failed.push(why);
                continue;
            }
        };
        r = match r.placed_by_world_file(&wf, v.crs) {
            Ok(mut placed) => {
                let others = count - 1;
                if others > 0 {
                    let by = if failed.is_empty() { "the largest" } else { "the largest that can be used" };
                    placed.lost.push(format!("it has {others} more map frames with coordinates (insets, say): it is placed by {by}"));
                }
                let unusable = failed.len() + unread.count;
                if let Some(why) = failed.first().or(unread.first.as_ref()) {
                    placed.lost.push(format!("{unusable} of its map frames have coordinates that can't be used ({why}, the first)"));
                }
                placed.lost.extend(v.note);
                if off > 1.0 {
                    placed.lost.push(format!("its map frame's corners don't lie on one flat grid: placed to within {off:.0} pixels"));
                }
                return placed;
            }
            Err((back, why)) => {
                failed.push(why);
                *back
            }
        };
    }
    if let Some(why) = failed.first().or(unread.first.as_ref()) {
        r.lost.push(format!("{} ({why}, the first): {unplaced}", unusable_frames(failed.len() + unread.count)));
    }
    r
}

/// That `n` of a page's map frames have coordinates that can't be used, said with the count.
fn unusable_frames(n: usize) -> String {
    if n == 1 {
        "1 map frame has coordinates that can't be used".into()
    } else {
        format!("{n} map frames have coordinates that can't be used")
    }
}

/// The world file placing a `w` × `h` drawing of the page by viewport `v` (`to_pixels` takes a
/// point on the page to the drawing's pixels): the least-squares affine through its control
/// points in its coordinate system, and how far, in pixels, the farthest lies from it.
fn fit(v: &Viewport, to_pixels: Affine, w: u32, h: u32) -> std::result::Result<(world::WorldFile, f64), String> {
    let t = v.crs.transformer()?;
    let [x0, y0, x1, y1] = v.bbox;
    // Each control point's pixel and its WGS 84 longitude/latitude.
    let mut at = Vec::with_capacity(v.gpts.len());
    for (&(lat, lon), &(lx, ly)) in v.gpts.iter().zip(&v.lpts) {
        let on = Coord::new(lon, lat);
        let wgs84 = match &v.def {
            Some(def) => crs::lonlat_from_datum_of(def, on).ok_or("a control point is off the Earth")?,
            None => on,
        };
        let p = to_pixels * Point::new(x0 + lx * (x1 - x0), y0 + ly * (y1 - y0));
        at.push(((p.x, p.y), wgs84));
    }
    // The frame's longitudes made continuous across it, taking the points left to right on the
    // page, whatever order the file gives them in: across 180°, 179° and -179° are 2° apart, not
    // 358°, and a frame with points at -170°, 0° and 170° stays 340° wide.
    at.sort_by(|a, b| a.0.0.total_cmp(&b.0.0).then(a.0.1.total_cmp(&b.0.1)));
    let mut previous: Option<f64> = None;
    for (_, c) in &mut at {
        if let Some(reference) = previous {
            c.x = reference + (c.x - reference + 180.0).rem_euclid(360.0) - 180.0;
        }
        previous = Some(c.x);
    }
    let mut points = Vec::with_capacity(at.len());
    for (p, wgs84) in at {
        let ground = t.from_lonlat(wgs84).ok_or("a control point is outside its coordinate system")?;
        points.push((p, (ground.x, ground.y)));
    }
    let [a, b, c] = solve(&points, |g| g.0).ok_or("its control points lie in a line")?;
    let [d, e, f] = solve(&points, |g| g.1).ok_or("its control points lie in a line")?;
    let size = (a * e - b * d).abs().sqrt();
    if !(size.is_normal() && [a, b, c, d, e, f].iter().all(|v| v.is_finite())) {
        return Err("its control points place the page nowhere".into());
    }
    let off =
        points.iter().map(|&((px, py), (gx, gy))| (a * px + b * py + c - gx).hypot(d * px + e * py + f - gy) / size).fold(0.0, f64::max);
    // A turn of less than a thousandth of a pixel across the whole drawing is none: the picture is
    // then placed north up as drawn, not resampled.
    let (w, h) = (f64::from(w), f64::from(h));
    let (b, d) = if (b * h).abs() < 1e-3 * a.abs() && (d * w).abs() < 1e-3 * e.abs() { (0.0, 0.0) } else { (b, d) };
    // The world file gives the centre of the top-left pixel.
    Ok((world::WorldFile { a, d, b, e, c: c + (a + b) / 2.0, f: f + (d + e) / 2.0 }, off))
}

/// The least-squares `[p, q, r]` with `p·x + q·y + r` nearest `value(ground)` at each `(x, y)`.
fn solve(points: &[((f64, f64), (f64, f64))], value: impl Fn((f64, f64)) -> f64) -> Option<[f64; 3]> {
    // The normal equations, solved by Cramer's rule. Pixel positions are centred first, so the
    // sums stay well conditioned on a large page.
    let n = points.len() as f64;
    let (mx, my) = points.iter().fold((0.0, 0.0), |(sx, sy), ((x, y), _)| (sx + x / n, sy + y / n));
    let (mut sxx, mut sxy, mut syy, mut sxv, mut syv, mut sv) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for &((x, y), g) in points {
        let (x, y, v) = (x - mx, y - my, value(g));
        sxx += x * x;
        sxy += x * y;
        syy += y * y;
        sxv += x * v;
        syv += y * v;
        sv += v;
    }
    let det = sxx * syy - sxy * sxy;
    if det.abs() <= 1e-12 * (sxx * syy).max(f64::MIN_POSITIVE) {
        return None;
    }
    let p = (sxv * syy - syv * sxy) / det;
    let q = (syv * sxx - sxv * sxy) / det;
    let r = sv / n - p * mx - q * my;
    Some([p, q, r])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fit_through_exact_points_is_exact_and_points_in_a_line_have_none() {
        // 2 units a pixel east, 3 south, from (100, 500).
        let at = |x: f64, y: f64| ((x, y), (100.0 + 2.0 * x, 500.0 - 3.0 * y));
        let pts = [at(0.0, 0.0), at(10.0, 0.0), at(10.0, 20.0), at(0.0, 20.0)];
        let [a, b, c] = solve(&pts, |g| g.0).unwrap();
        let [d, e, f] = solve(&pts, |g| g.1).unwrap();
        for (got, want) in [a, b, c, d, e, f].iter().zip([2.0, 0.0, 100.0, 0.0, -3.0, 500.0]) {
            assert!((got - want).abs() < 1e-9, "{got} for {want}");
        }
        assert!(solve(&[at(0.0, 0.0), at(1.0, 1.0), at(2.0, 2.0)], |g| g.0).is_none());
        assert!(super::super::is_pdf_name("/d/Map.PDF") && !super::super::is_pdf_name("map.pdf.tif"));
    }

    /// A one-page PDF of `media` points with a blue square drawn, its page dictionary given
    /// `extra` (a `/VP`, say), with a correct cross-reference table.
    fn tiny_pdf(media: &str, extra: &str) -> Vec<u8> {
        let content = "0 0 1 rg 0 0 10 10 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [{media}] /Contents 4 0 R {extra} >>"),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        ];
        let mut out = String::from("%PDF-1.7\n");
        let mut at = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            at.push(out.len());
            out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
        }
        let xref = out.len();
        out.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
        for a in at {
            out.push_str(&format!("{a:010} 00000 n \n"));
        }
        out.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1));
        out.into_bytes()
    }

    /// A viewport over `bbox` on the page, its corners at 10–12° E, 40–41° N.
    fn vp(gcs: &str, bbox: &str) -> String {
        format!(
            "<< /Type /Viewport /BBox [{bbox}] /Measure << /Type /Measure /Subtype /GEO \
             /GPTS [41 10 41 12 40 12 40 10] /LPTS [0 1 1 1 1 0 0 0] /GCS << {gcs} >> >> >>"
        )
    }

    /// A viewport over the whole of a 72 × 36 point page.
    fn geo(gcs: &str) -> String {
        format!("/VP [{}]", vp(gcs, "0 0 72 36"))
    }

    #[test]
    fn a_page_without_coordinates_is_a_scan_drawn_at_150_dpi() {
        let r = read_bytes("plain", tiny_pdf("0 0 72 36", ""), None, true).unwrap();
        assert_eq!((r.width, r.height, r.bands.len(), r.georeferenced), (150, 75, 3, false));
        assert!(r.lost.is_empty(), "{:?}", r.lost);
        // The blue square at the bottom left, white elsewhere.
        let at = |band: usize, col: usize, row: usize| r.bands[band].data[row * 150 + col];
        assert_eq!((at(0, 2, 72), at(1, 2, 72), at(2, 2, 72)), (0.0, 0.0, 255.0));
        assert_eq!((at(0, 100, 10), at(2, 100, 10)), (255.0, 255.0));
        let e = read_bytes("plain", tiny_pdf("0 0 72 36", ""), Some("2"), true).unwrap_err().to_string();
        assert!(e.contains("from 1 to 1"), "{e}");
    }

    #[test]
    fn a_geopdf_page_is_placed_by_its_viewport() {
        let wgs84 = r#"/Type /GEOGCS /EPSG 4326 /WKT (GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]])"#;
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &geo(wgs84)), None, true).unwrap();
        assert!(r.georeferenced && r.lost.is_empty(), "{:?}", r.lost);
        let e = r.extent();
        for (got, want) in [(e.min_x, 10.0), (e.max_x, 12.0), (e.min_y, 40.0), (e.max_y, 41.0)] {
            assert!((got - want).abs() < 1e-9, "{e:?}");
        }
        // A code RustGIS doesn't know: its WKT is read instead.
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &geo(&wgs84.replace("4326", "999999"))), None, true).unwrap();
        assert!(r.georeferenced, "{:?}", r.lost);
        // A second, smaller frame (an inset) is counted, and the larger places the page; read as
        // a scan, nothing is said of either.
        let both = format!("/VP [{} {}]", vp(wgs84, "0 0 72 36"), vp(wgs84, "0 0 10 10"));
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &both), None, true).unwrap();
        assert!(
            r.georeferenced && r.lost == ["it has 1 more map frames with coordinates (insets, say): it is placed by the largest"],
            "{:?}",
            r.lost
        );
        assert!((r.extent().max_x - 12.0).abs() < 1e-9, "placed by the larger");
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &both), None, false).unwrap();
        assert!(!r.georeferenced && r.lost.is_empty(), "{:?}", r.lost);
        // A frame across 180°: two degrees wide, not mirrored over the rest of the world.
        let across = geo(wgs84).replace("/GPTS [41 10 41 12 40 12 40 10]", "/GPTS [41 179 41 -179 40 -179 40 179]");
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &across), None, true).unwrap();
        let wide = r.transform.pixel_width * f64::from(r.width);
        assert!(r.georeferenced && (wide - 2.0).abs() < 1e-6, "{wide}° wide, {:?}", r.lost);
        // A frame round most of the world, its points stepping across it: 340° wide.
        let wide = geo(wgs84).replace(
            "/GPTS [41 10 41 12 40 12 40 10] /LPTS [0 1 1 1 1 0 0 0]",
            "/GPTS [41 -170 41 0 41 170 40 170 40 0 40 -170] /LPTS [0 1 0.5 1 1 1 1 0 0.5 0 0 0]",
        );
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &wide), None, true).unwrap();
        let span = r.transform.pixel_width * f64::from(r.width);
        assert!(r.georeferenced && (span - 340.0).abs() < 1e-6, "{span}° wide, {:?}", r.lost);
        // The same frame with its pairs in another order: still 340° wide.
        let shuffled = geo(wgs84).replace(
            "/GPTS [41 10 41 12 40 12 40 10] /LPTS [0 1 1 1 1 0 0 0]",
            "/GPTS [41 -170 40 170 41 0 40 0 41 170 40 -170] /LPTS [0 1 1 0 0.5 1 0.5 0 1 1 0 0]",
        );
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &shuffled), None, true).unwrap();
        let span = r.transform.pixel_width * f64::from(r.width);
        assert!(r.georeferenced && (span - 340.0).abs() < 1e-6, "{span}° wide, {:?}", r.lost);
        // A larger frame whose points lie in a line gives way to a smaller one that places it.
        let flat = vp(wgs84, "0 0 72 36").replace("/LPTS [0 1 1 1 1 0 0 0]", "/LPTS [0 0 0.3 0.3 0.6 0.6 1 1]");
        let both = format!("/VP [{flat} {}]", vp(wgs84, "0 0 10 10"));
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &both), None, true).unwrap();
        assert!(r.georeferenced, "{:?}", r.lost);
        assert!(r.lost.iter().any(|t| t.contains("placed by the largest that can be used")), "{:?}", r.lost);
        assert!(r.lost.iter().any(|t| t.starts_with("1 of its map frames have coordinates that can't be used")), "{:?}", r.lost);
        // Without /LPTS, /Bounds (the frame's outline) isn't taken for the points' places: a scan,
        // and said.
        let outline = geo(wgs84).replace("/LPTS [0 1 1 1 1 0 0 0]", "/Bounds [0 1 1 1 1 0 0 0]");
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &outline), None, true).unwrap();
        assert!(!r.georeferenced && r.lost[0].contains("(/LPTS)"), "{:?}", r.lost);
        // Two frames, neither usable, and an entry that isn't a frame: all three counted.
        let none = format!("/VP [{} {} 7]", vp("/Type /GEOGCS", "0 0 72 36"), vp("/Type /GEOGCS", "0 0 10 10"));
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &none), None, true).unwrap();
        assert!(!r.georeferenced && r.lost[0].starts_with("3 map frames have coordinates that can't be used ("), "{:?}", r.lost);
        // A rectangle with a fifth entry: not read as four corners, and said.
        let five = geo(wgs84).replace("/BBox [0 0 72 36]", "/BBox [0 0 72 36 /extra]");
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &five), None, true).unwrap();
        assert!(!r.georeferenced && r.lost[0].contains("isn't four numbers"), "{:?}", r.lost);
        // A control point that isn't a number: no placing by pairs out of step, and said.
        let bad = geo(wgs84).replace("/GPTS [41 10 41 12", "/GPTS [41 /Ten 41 12");
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &bad), None, true).unwrap();
        assert!(!r.georeferenced && r.lost[0].contains("aren't pairs of numbers"), "{:?}", r.lost);
        // No system at all: a scan, and said.
        let r = read_bytes("geo", tiny_pdf("0 0 72 36", &geo("/Type /GEOGCS")), None, true).unwrap();
        assert!(!r.georeferenced && r.lost[0].contains("doesn't say its coordinate system"), "{:?}", r.lost);
    }

    #[test]
    fn hostile_pdfs_are_refused_or_drawn_small_never_a_panic() {
        assert!(read_bytes("x", b"not a pdf".to_vec(), None, true).unwrap_err().to_string().contains("not a PDF"));
        // Cut short: whatever it reads, it doesn't panic.
        let whole = tiny_pdf("0 0 72 36", "");
        for cut in [10, whole.len() / 2, whole.len() - 8] {
            let _ = read_bytes("cut", whole[..cut].to_vec(), None, true);
        }
        // A page ten million points on a side is drawn within the limit, and says so.
        let r = read_limited("huge", tiny_pdf("0 0 10000000 10000000", ""), None, true, 30_000).unwrap();
        assert!(r.width * r.height * 3 <= 30_000 && r.width >= 1, "{} × {}", r.width, r.height);
        assert!(r.lost[0].contains("dots an inch, not 150"), "{:?}", r.lost);
        // A page box of no area: drawn at a size the renderer gives it, or refused, never a panic.
        let _ = read_bytes("flat", tiny_pdf("0 0 72 0", ""), None, true);
    }
}
