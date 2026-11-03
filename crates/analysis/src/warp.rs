//! Warping a raster onto a north-up grid through a control-point fit ([`crate::georef::Fit`]), and
//! projecting one onto a grid of another coordinate system, cell size and extent ([`project`]).
//!
//! `GeoTransform` has no rotation terms, so an image that a fit turns, shears or bends can only be
//! placed by resampling it. Every cell of the output grid is taken back through the fit (after
//! reprojection, when the output's coordinate system isn't the one the control points are in) to a
//! position in the image and sampled there; cells whose position falls outside the image are
//! no data.
//!
//! The way back to the image is computed exactly at every [`STEP`]th cell of a row and linearly
//! between, as GDAL's approximate transformer does: where the midpoint of a stretch is off by more
//! than [`TOLERANCE`] image pixels, the whole stretch is computed exactly.

use std::collections::HashMap;

use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster, Transformer, max_samples};
use rustgis_geom::Coord;

use crate::georef::Fit;

/// Cells between exact computations of the way back to the image.
const STEP: usize = 8;
/// The most, in image pixels, a linearly placed midpoint may be off.
const TOLERANCE: f64 = 0.125;
/// Points along each side of the image when its footprint is measured.
const EDGE_POINTS: usize = 64;
/// Divisions of the image each way for the lattice of points inside it.
const INTERIOR_POINTS: usize = 32;
/// What one output column costs in working buffers, in `f32` samples: its image position (24
/// bytes) and its share of the exact nodes (4 bytes).
const COLUMN_COST: f64 = 7.0;

/// How cells are sampled from the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resampling {
    /// The pixel the position falls in: values are kept, which is right for classes.
    Nearest,
    /// The four nearest pixel centres, by distance.
    Bilinear,
    /// Catmull-Rom over the sixteen nearest, kept within the band's range; where one of them has
    /// no value, bilinear.
    Cubic,
}

impl Resampling {
    pub fn parse(s: &str) -> Option<Resampling> {
        match s.trim().to_ascii_lowercase().as_str() {
            "nearest" => Some(Resampling::Nearest),
            "bilinear" => Some(Resampling::Bilinear),
            "cubic" => Some(Resampling::Cubic),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Resampling::Nearest => "nearest",
            Resampling::Bilinear => "bilinear",
            Resampling::Cubic => "cubic",
        }
    }
}

/// What to warp onto.
#[derive(Clone, Debug)]
pub struct Options {
    /// The output grid's coordinate system.
    pub crs: Crs,
    /// The coordinate system of the fit's map coordinates.
    pub points_crs: Crs,
    pub resampling: Resampling,
    /// Cell size in the output's units; about the image pixel's size on the ground when `None`.
    pub cell: Option<f64>,
    pub name: String,
}

/// Map coordinates of the fit, to and from the output's coordinate system.
struct Reproject {
    from: Transformer,
    to: Transformer,
    same: bool,
}

impl Reproject {
    fn new(points: Crs, out: Crs) -> Result<Reproject, String> {
        Ok(Reproject { from: points.transformer()?, to: out.transformer()?, same: points == out })
    }

    /// Fit coordinates to output coordinates.
    fn forward(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        if self.same {
            return (x.is_finite() && y.is_finite()).then_some((x, y));
        }
        let c = self.to.from_lonlat(self.from.to_lonlat(Coord::new(x, y))?)?;
        Some((c.x, c.y))
    }

    /// Output coordinates to fit coordinates.
    fn back(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        if self.same {
            return (x.is_finite() && y.is_finite()).then_some((x, y));
        }
        let c = self.from.from_lonlat(self.to.to_lonlat(Coord::new(x, y))?)?;
        Some((c.x, c.y))
    }
}

/// Reads pixels at fractional image positions.
struct Sampler<'a> {
    r: &'a Raster,
    method: Resampling,
    /// The band whose value weighs the others (alpha), if the image has one.
    alpha: Option<usize>,
    /// Whole-number pixel types are rounded.
    round: bool,
}

impl Sampler<'_> {
    /// The value of band `b` at index `idx`, if it is a real measurement.
    fn at(&self, b: usize, idx: usize) -> Option<f32> {
        let v = *self.r.bands.get(b)?.data.get(idx)?;
        self.r.is_valid(v).then_some(v)
    }

    /// Every band at image position (`c`, `row`), in pixels from the top left corner; NaN where
    /// there's no value. The position must be inside the image.
    fn sample(&self, c: f64, row: f64, out: &mut [f32]) {
        let (w, h) = (i64::from(self.r.width), i64::from(self.r.height));
        let index = |i: i64, j: i64| (j.clamp(0, h - 1) * w + i.clamp(0, w - 1)) as usize;
        // The pixel under the position. The 1e-10 (as in GDAL) keeps a position a rounding error
        // below a pixel's edge in the pixel after it.
        let centre = index((c + 1e-10).floor() as i64, (row + 1e-10).floor() as i64);
        if self.method == Resampling::Nearest {
            for (b, o) in out.iter_mut().enumerate() {
                *o = self.at(b, centre).unwrap_or(f32::NAN);
            }
            return;
        }
        // Pixel centres are at half integers.
        let (u, v) = (c - 0.5, row - 0.5);
        let (i0, j0) = (u.floor() as i64, v.floor() as i64);
        let (fu, fv) = (u - i0 as f64, v - j0 as f64);
        // A position on a pixel with no value has none either, as in GDAL, rather than the mean of
        // the neighbours that have one (#973). That includes NaN in a raster without a no-data
        // value (gdalwarp with `-srcnodata nan`). The pixel has no value only where every band
        // but alpha is no-data, as gdalwarp's default (`UNIFIED_SRC_NODATA=PARTIAL`) has it: a
        // colour pixel with one channel at the no-data value keeps its value, and that channel
        // blends from the neighbours. At the pixel's exact centre only that pixel weighs, so the
        // channel has no value there, as in gdalwarp.
        if (0..out.len()).all(|b| Some(b) == self.alpha || self.at(b, centre).is_none()) {
            out.fill(f32::NAN);
            return;
        }
        for (b, o) in out.iter_mut().enumerate() {
            let cubic = (self.method == Resampling::Cubic).then(|| self.cubic(b, &index, i0, j0, fu, fv)).flatten();
            *o = cubic.unwrap_or_else(|| self.bilinear(b, &index, i0, j0, fu, fv));
            if self.round && o.is_finite() {
                *o = o.round();
            }
        }
    }

    fn bilinear(&self, b: usize, index: &dyn Fn(i64, i64) -> usize, i0: i64, j0: i64, fu: f64, fv: f64) -> f32 {
        let weighted = self.alpha.filter(|a| *a != b);
        let (mut sum, mut weight) = (0.0f64, 0.0f64);
        for (di, dj, w) in [(0, 0, (1.0 - fu) * (1.0 - fv)), (1, 0, fu * (1.0 - fv)), (0, 1, (1.0 - fu) * fv), (1, 1, fu * fv)] {
            let idx = index(i0 + di, j0 + dj);
            let Some(v) = self.at(b, idx) else { continue };
            // Colours count as much as their pixel is opaque.
            let k = weighted.map_or(1.0, |a| self.at(a, idx).map_or(0.0, f64::from));
            sum += f64::from(v) * w * k;
            weight += w * k;
        }
        if weight > 1e-12 { (sum / weight) as f32 } else { f32::NAN }
    }

    /// Catmull-Rom over the sixteen pixels around the position; `None` where one has no value,
    /// and for a colour band unless all sixteen have the same alpha above 0 (bilinear, which
    /// weighs colours by alpha, takes the rest).
    fn cubic(&self, b: usize, index: &dyn Fn(i64, i64) -> usize, i0: i64, j0: i64, fu: f64, fv: f64) -> Option<f32> {
        let kernel = |t: f64| {
            [
                (-t * t * t + 2.0 * t * t - t) / 2.0,
                (3.0 * t * t * t - 5.0 * t * t + 2.0) / 2.0,
                (-3.0 * t * t * t + 4.0 * t * t + t) / 2.0,
                (t * t * t - t * t) / 2.0,
            ]
        };
        let (wx, wy) = (kernel(fu), kernel(fv));
        let weighted = self.alpha.filter(|a| *a != b);
        let mut alpha = None;
        let mut sum = 0.0f64;
        for (dj, wj) in wy.iter().enumerate() {
            for (di, wi) in wx.iter().enumerate() {
                let idx = index(i0 + di as i64 - 1, j0 + dj as i64 - 1);
                if let Some(a) = weighted {
                    // A see-through pixel's colour mustn't count.
                    let k = self.at(a, idx)?;
                    if k <= 0.0 || *alpha.get_or_insert(k) != k {
                        return None;
                    }
                }
                let v = self.at(b, idx)?;
                sum += f64::from(v) * wi * wj;
            }
        }
        // Kept within the band's range (a range that is missing or reversed leaves it be).
        let s = &self.r.bands.get(b)?.stats;
        Some(if s.min <= s.max { sum.clamp(s.min, s.max) } else { sum } as f32)
    }
}

/// Warps `r` onto a north-up grid in `o.crs` through `fit`, whose pixel positions are those of
/// `r` (in the full image's pixels, when `r` is an overview of it) and whose map coordinates are
/// in `o.points_crs`. The grid covers the warped image's footprint. A colour or grey byte raster
/// with neither alpha nor no-data gets an alpha band (0–255), so the cells outside the image are
/// see-through. Geographic output cells are square in degrees.
pub fn warp(r: &Raster, fit: &Fit, o: &Options) -> Result<Raster, String> {
    let re = Reproject::new(o.points_crs, o.crs)?;
    let (w, h) = (f64::from(r.width), f64::from(r.height));
    // An overview's pixels are bigger than the full image's, which the fit is in.
    let (sx, sy) = r.overview.map_or((1.0, 1.0), |(fw, fh)| (f64::from(fw) / w, f64::from(fh) / h));
    let place = |c: f64, row: f64| fit.to_map(c * sx, row * sy).and_then(|(x, y)| re.forward(x, y));

    let [min_x, min_y, max_x, max_y] = footprint(w, h, &place).ok_or("the image's edge can't be placed in the output coordinate system")?;
    if o.crs.is_geographic() && max_x - min_x > 180.0 {
        return Err("the warped image spans more than 180° of longitude (it crosses the antimeridian, or the fit sends part of it far away): give a projected crs".into());
    }
    if max_x <= min_x || max_y <= min_y {
        return Err("the fit puts the image on a line, with no area".into());
    }
    let canvas = Canvas::of(r, true);
    let out_bands = canvas.out_bands;

    // The cell: asked for, or the image pixel's size on the ground (the root of its area, where
    // the fit turns or skews it), coarser if the grid would hold too many samples.
    let cells = |cell: f64| -> (f64, f64) { (((max_x - min_x) / cell).ceil().max(1.0), ((max_y - min_y) / cell).ceil().max(1.0)) };
    // The samples held, and the buffers a row needs (they add up on a long thin grid).
    let load = |(ow, oh): (f64, f64)| ow * (oh * out_bands as f64 + COLUMN_COST);
    let max = max_samples();
    let too_many = |n: (f64, f64)| load(n) > max as f64;
    let cell = match o.cell {
        Some(c) if !(c.is_finite() && c > 0.0) => return Err("cellSize must be a positive number".into()),
        Some(c) => {
            if too_many(cells(c)) {
                return Err(format!(
                    "a cell size of {c} makes a grid of more than {max} samples, this computer's limit: give a larger cellSize"
                ));
            }
            c
        }
        None => {
            let mut cell = pixel_size(w, h, &place).ok_or("the source pixel's size can't be worked out: give a cellSize")?;
            // Each step at least 1% coarser, so a grid rounded up past the limit still shrinks.
            for _ in 0..64 {
                let n = cells(cell);
                if !too_many(n) {
                    break;
                }
                cell *= (load(n) / max as f64).sqrt().max(1.01);
            }
            if too_many(cells(cell)) {
                return Err("the warped image is too large to hold: give a cellSize".into());
            }
            cell
        }
    };
    let (ow, oh) = cells(cell);
    let g = Grid { crs: o.crs, min_x, max_y, cell_x: cell, cell_y: cell, width: ow as usize, height: oh as usize };
    // The image position of an output coordinate.
    let source = |x: f64, y: f64| -> Option<(f64, f64)> {
        let (px, py) = re.back(x, y)?;
        let (c, row) = fit.to_pixel(px, py)?;
        let (c, row) = (c / sx, row / sy);
        (c.is_finite() && row.is_finite()).then_some((c, row))
    };
    sampled(r, &g, canvas, o.resampling, &source, o.name.clone())
}

/// The bounds `[min x, min y, max x, max y]` of a `w` × `h` image whose pixel positions `place`
/// puts in output coordinates: its edge and a lattice inside it (a pole inside the image is no
/// extreme of its edge). `None` when too little of it can be placed.
fn footprint(w: f64, h: f64, place: &dyn Fn(f64, f64) -> Option<(f64, f64)>) -> Option<[f64; 4]> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    let mut placed = 0;
    let mut add = |c: f64, row: f64| {
        if let Some((x, y)) = place(c, row) {
            (min_x, min_y, max_x, max_y) = (min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y));
            placed += 1;
        }
    };
    for i in 0..=EDGE_POINTS {
        let t = i as f64 / EDGE_POINTS as f64;
        for (c, row) in [(t * w, 0.0), (t * w, h), (0.0, t * h), (w, t * h)] {
            add(c, row);
        }
    }
    for i in 0..=INTERIOR_POINTS {
        for j in 0..=INTERIOR_POINTS {
            add(i as f64 / INTERIOR_POINTS as f64 * w, j as f64 / INTERIOR_POINTS as f64 * h);
        }
    }
    (placed >= 4 && [min_x, min_y, max_x, max_y].iter().all(|v| v.is_finite())).then_some([min_x, min_y, max_x, max_y])
}

/// About the size of a pixel of a `w` × `h` image in output coordinates, at its centre: the root
/// of its area, where `place` turns or skews it.
fn pixel_size(w: f64, h: f64, place: &dyn Fn(f64, f64) -> Option<(f64, f64)>) -> Option<f64> {
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (a, b, d) = (place(cx, cy)?, place(cx + 1.0, cy)?, place(cx, cy + 1.0)?);
    let cell = ((b.0 - a.0) * (d.1 - a.1) - (d.0 - a.0) * (b.1 - a.1)).abs().sqrt();
    (cell.is_finite() && cell > 0.0).then_some(cell)
}

/// A north-up grid: its coordinate system, the top left corner of its top left cell, its cell
/// size each way and how many cells it has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub crs: Crs,
    pub min_x: f64,
    pub max_y: f64,
    pub cell_x: f64,
    pub cell_y: f64,
    pub width: usize,
    pub height: usize,
}

impl Grid {
    /// The grid `r` is on.
    pub fn of(r: &Raster) -> Grid {
        let t = r.transform;
        let (w, h) = (f64::from(r.width), f64::from(r.height));
        Grid {
            crs: r.crs,
            min_x: t.origin_x.min(t.origin_x + w * t.pixel_width),
            max_y: t.origin_y.max(t.origin_y + h * t.pixel_height),
            cell_x: t.pixel_width.abs(),
            cell_y: t.pixel_height.abs(),
            width: r.width as usize,
            height: r.height as usize,
        }
    }

    /// Cells of `cell` (x, y) in `crs` covering `bounds` (`[min x, min y, max x, max y]`), their
    /// corners on `origin` plus whole cells, so grids with the same origin and cell line up.
    pub fn snapped(crs: Crs, bounds: [f64; 4], cell: (f64, f64), origin: (f64, f64)) -> Result<Grid, String> {
        let [x0, y0, x1, y1] = bounds;
        let (cx, cy) = cell;
        if !(cx.is_finite() && cy.is_finite() && cx > 0.0 && cy > 0.0) {
            return Err("cellSize must be a positive number".into());
        }
        if !(origin.0.is_finite() && origin.1.is_finite()) {
            return Err("origin is [x, y] in the output's units".into());
        }
        if !(bounds.iter().all(|v| v.is_finite()) && x0 < x1 && y0 < y1) {
            return Err("the area has no extent in the output coordinate system".into());
        }
        let min_x = origin.0 + ((x0 - origin.0) / cx).floor() * cx;
        let max_x = origin.0 + ((x1 - origin.0) / cx).ceil() * cx;
        let min_y = origin.1 + ((y0 - origin.1) / cy).floor() * cy;
        let max_y = origin.1 + ((y1 - origin.1) / cy).ceil() * cy;
        // An origin or a cell so far from the area that the snapped corners overflow.
        if !([min_x, max_x, min_y, max_y].iter().all(|v| v.is_finite()) && min_x < max_x && min_y < max_y) {
            return Err("the origin and cellSize can't make a grid over this area".into());
        }
        let (w, h) = (((max_x - min_x) / cx).round().max(1.0), ((max_y - min_y) / cy).round().max(1.0));
        let max = max_samples();
        if w * h > max as f64 {
            return Err(format!(
                "a cell size of {cx} makes a grid of {} × {} cells, more than this computer's {max}: give a larger cellSize or a smaller extent",
                w as u64, h as u64
            ));
        }
        Ok(Grid { crs, min_x, max_y, cell_x: cx, cell_y: cy, width: w as usize, height: h as usize })
    }

    /// The top left corner's transform.
    fn transform(&self) -> GeoTransform {
        GeoTransform { origin_x: self.min_x, origin_y: self.max_y, pixel_width: self.cell_x, pixel_height: -self.cell_y }
    }
}

/// The bounds in `crs` of `r`'s footprint, and about the size of its pixel there.
pub fn footprint_in(r: &Raster, crs: Crs) -> Result<([f64; 4], f64), String> {
    r.require_coordinates()?;
    let re = Reproject::new(r.crs, crs)?;
    let t = r.transform;
    let place = |c: f64, row: f64| {
        let p = t.to_world(c, row);
        re.forward(p.x, p.y)
    };
    let (w, h) = (f64::from(r.width), f64::from(r.height));
    let bounds = footprint(w, h, &place).ok_or_else(|| format!("{} can't be placed in {}", r.name, crs.label()))?;
    let cell = pixel_size(w, h, &place).ok_or_else(|| format!("the size of {}'s pixel can't be worked out: give a cellSize", r.name))?;
    Ok((bounds, cell))
}

/// The bounds in `crs` of a longitude/latitude box `[west, south, east, north]`, its edges
/// followed (a projection bends them).
pub fn bounds_in(lonlat: [f64; 4], crs: Crs) -> Result<[f64; 4], String> {
    let [west, south, east, north] = lonlat;
    let re = Reproject::new(Crs::LonLat, crs)?;
    let place = |u: f64, v: f64| re.forward(west + u * (east - west), north - v * (north - south));
    footprint(1.0, 1.0, &place).ok_or_else(|| format!("the area can't be placed in {}", crs.label()))
}

/// How [`project`] fills a cell: sampled at its centre, or from every source pixel whose centre
/// falls in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Sampled at the cell's centre.
    Sample(Resampling),
    /// The mean of the source pixels in the cell.
    Average,
    /// The most frequent value of the source pixels in the cell; of those tied, the one that
    /// reaches the top count first, the pixels taken row by row (as GDAL's `mode`).
    Mode,
}

impl Method {
    pub fn parse(s: &str) -> Option<Method> {
        match s.trim().to_ascii_lowercase().as_str() {
            "average" | "mean" => Some(Method::Average),
            "mode" | "majority" => Some(Method::Mode),
            other => Resampling::parse(other).map(Method::Sample),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Method::Sample(r) => r.id(),
            Method::Average => "average",
            Method::Mode => "mode",
        }
    }

    /// The method `r` is projected with: a palette image's classes are never mixed, so it is
    /// sampled at the nearest pixel, or takes the mode, whatever was asked.
    pub fn used_on(self, r: &Raster) -> Method {
        match (self, &r.color) {
            (Method::Average | Method::Mode, ColorHint::Palette(_)) => Method::Mode,
            (Method::Sample(_), ColorHint::Palette(_)) => Method::Sample(Resampling::Nearest),
            (m, _) => m,
        }
    }
}

/// Projects `r` onto the grid `g`, through longitude/latitude when their coordinate systems
/// differ: each cell sampled at its centre, or (average, mode) taken from every pixel of `r`
/// whose centre falls in it. A cell no pixel centre falls in (a grid finer than the raster) takes
/// the pixel under its centre. Cells off the raster are its no-data (else NaN, or 0 in a
/// whole-number type), or see-through for a colour image with neither alpha nor no-data, which
/// gains an alpha band for it.
pub fn project(r: &Raster, g: &Grid, method: Method, name: String) -> Result<Raster, String> {
    r.require_coordinates()?;
    let canvas = Canvas::of(r, false);
    let out_bands = canvas.out_bands;
    // The samples held, and the buffers a row needs.
    let max = max_samples();
    if g.width as f64 * (g.height as f64 * out_bands as f64 + COLUMN_COST) > max as f64 {
        return Err(format!(
            "{} × {} cells of {} band(s) are more than this computer's {max} samples: give a larger cellSize or a smaller extent",
            g.width, g.height, out_bands
        ));
    }
    let re = Reproject::new(r.crs, g.crs)?;
    let t = r.transform;
    let source = |x: f64, y: f64| -> Option<(f64, f64)> {
        let (px, py) = re.back(x, y)?;
        let (c, row) = t.to_pixel(Coord::new(px, py));
        (c.is_finite() && row.is_finite()).then_some((c, row))
    };
    // The method that runs, as the result reports it (a palette image's classes are never mixed).
    match method.used_on(r) {
        Method::Sample(m) => sampled(r, g, canvas, m, &source, name),
        Method::Average | Method::Mode => aggregated(r, g, canvas, method == Method::Mode, &source, name),
    }
}

/// How an output raster holds a source's bands.
struct Canvas {
    /// The source's bands.
    nb: usize,
    /// Its alpha band, if it has one.
    alpha_src: Option<usize>,
    /// Whether an alpha band is added: a colour image (or, for a picture, a grey byte one) with
    /// neither alpha nor no-data, so the cells outside the image are see-through.
    add_alpha: bool,
    out_bands: usize,
    /// Whether a cell with no value is see-through (zero) rather than no-data.
    clear: bool,
    /// What a cell with no value holds.
    outside: f32,
    color: ColorHint,
}

impl Canvas {
    /// Its output bands on a `w × h` grid, every cell `outside` ([`crate::rasterops::new_band`]).
    fn bands(&self, w: usize, h: usize) -> Result<Vec<Vec<f32>>, String> {
        (0..self.out_bands).map(|_| crate::rasterops::new_band(w, h, self.outside)).collect()
    }

    /// How `r` is held: as a `picture` (a scan being georeferenced), a grey byte image gains
    /// alpha as a colour one does; as data, it keeps its one band, and a cell with no value is
    /// its no-data, else NaN in a floating-point type and 0 in a whole-number one, as GDAL leaves
    /// it.
    fn of(r: &Raster, picture: bool) -> Canvas {
        let nb = r.bands.len();
        let alpha_src = match r.color {
            ColorHint::Rgb { alpha: true } if nb >= 4 => Some(3),
            ColorHint::GrayAlpha if nb >= 2 => Some(1),
            _ => None,
        };
        let rgb = matches!(r.color, ColorHint::Rgb { .. }) && nb >= 3;
        let gray = picture && matches!(r.color, ColorHint::Gray) && nb == 1 && r.data_type == "UInt8";
        let add_alpha = (rgb || gray) && alpha_src.is_none() && r.nodata.is_none();
        // Cells with no value: no-data where the raster has some. Colours with alpha are zero there.
        let clear = alpha_src.is_some() || add_alpha;
        let none = if picture || r.data_type.starts_with("Float") { f32::NAN } else { 0.0 };
        let outside = if clear { 0.0 } else { r.nodata.map_or(none, |d| d as f32) };
        let color = match (add_alpha, rgb) {
            (true, true) => ColorHint::Rgb { alpha: true },
            (true, false) => ColorHint::GrayAlpha,
            _ => r.color.clone(),
        };
        Canvas { nb, alpha_src, add_alpha, out_bands: nb + usize::from(add_alpha), clear, outside, color }
    }

    /// Cell `at` of `bands` set to `values` (no value where NaN), and, when an alpha band was
    /// added, opaque unless no band has a value.
    fn set(&self, bands: &mut [Vec<f32>], at: usize, values: &[f32]) {
        for (band, v) in bands.iter_mut().zip(values) {
            if let Some(o) = band.get_mut(at) {
                // A cell with no value is no-data (or see-through) like one outside.
                *o = if v.is_nan() { if self.clear { 0.0 } else { self.outside } } else { *v };
            }
        }
        // Alpha is read as 0–255 whatever the pixel type. A cell with no value in any band (a
        // NaN pixel under its centre in a float image without no-data) is see-through like one
        // outside, not opaque black.
        if self.add_alpha
            && let Some(o) = bands.get_mut(self.nb).and_then(|b| b.get_mut(at))
        {
            *o = if values.iter().take(self.nb).all(|v| v.is_nan()) { 0.0 } else { 255.0 };
        }
    }

    /// The output raster on `g`, its bands named as the source's.
    fn finish(&self, r: &Raster, g: &Grid, bands: Vec<Vec<f32>>, name: String) -> Result<Raster, String> {
        let mut out = Raster::new(
            name,
            g.width as u32,
            g.height as u32,
            bands,
            g.transform(),
            g.crs,
            r.nodata,
            self.color.clone(),
            r.data_type.clone(),
        )
        .map_err(|e| e.to_string())?;
        for (i, band) in out.bands.iter_mut().enumerate() {
            band.name = r.bands.get(i).map_or_else(|| "Alpha".to_string(), |b| b.name.clone());
        }
        Ok(out)
    }
}

/// The image positions of the `out.len()` points `x_at(i)` along the row `y` of output
/// coordinates: exact every [`STEP`] points, and linear between where the midpoint of a stretch
/// is within [`TOLERANCE`] of exact (otherwise exact throughout the stretch).
fn row_positions(
    x_at: &dyn Fn(usize) -> f64,
    y: f64,
    source: &dyn Fn(f64, f64) -> Option<(f64, f64)>,
    nodes: &mut Vec<(usize, Option<(f64, f64)>)>,
    out: &mut [Option<(f64, f64)>],
) {
    let n = out.len();
    if n == 0 {
        return;
    }
    nodes.clear();
    let mut i = 0;
    loop {
        nodes.push((i, source(x_at(i), y)));
        if i + 1 >= n {
            break;
        }
        i = (i + STEP).min(n - 1);
    }
    for pair in nodes.windows(2) {
        let [(a, pa), (b, pb)] = pair else { continue };
        let linear = match (pa, pb) {
            (Some(pa), Some(pb)) if b - a >= 2 => {
                let mid = a + (b - a) / 2;
                let t = (mid - a) as f64 / (b - a) as f64;
                let guess = (pa.0 + (pb.0 - pa.0) * t, pa.1 + (pb.1 - pa.1) * t);
                source(x_at(mid), y).is_some_and(|m| (m.0 - guess.0).abs().max((m.1 - guess.1).abs()) <= TOLERANCE)
            }
            (Some(_), Some(_)) => true,
            _ => false,
        };
        for k in *a..*b {
            let t = (k - a) as f64 / (b - a) as f64;
            let c = match (linear, pa, pb) {
                (true, Some(pa), Some(pb)) => Some((pa.0 + (pb.0 - pa.0) * t, pa.1 + (pb.1 - pa.1) * t)),
                _ => source(x_at(k), y),
            };
            if let Some(slot) = out.get_mut(k) {
                *slot = c;
            }
        }
    }
    if let Some((last, p)) = nodes.last()
        && let Some(c) = out.get_mut(*last)
    {
        *c = *p;
    }
}

/// `r` on the grid `g`, each cell sampled at its centre's image position (`source`).
fn sampled(
    r: &Raster,
    g: &Grid,
    canvas: Canvas,
    method: Resampling,
    source: &dyn Fn(f64, f64) -> Option<(f64, f64)>,
    name: String,
) -> Result<Raster, String> {
    let (w, h) = (f64::from(r.width), f64::from(r.height));
    let (ow, oh) = (g.width, g.height);
    let method = if matches!(r.color, ColorHint::Palette(_)) { Resampling::Nearest } else { method };
    let sampler = Sampler { r, method, alpha: canvas.alpha_src, round: !r.data_type.starts_with("Float") };
    let mut bands = canvas.bands(ow, oh)?;
    let mut coords: Vec<Option<(f64, f64)>> = vec![None; ow];
    let mut nodes = Vec::new();
    let mut px = vec![f32::NAN; canvas.nb];
    let x_at = |ox: usize| g.min_x + (ox as f64 + 0.5) * g.cell_x;
    for oy in 0..oh {
        let y = g.max_y - (oy as f64 + 0.5) * g.cell_y;
        row_positions(&x_at, y, source, &mut nodes, &mut coords);
        for (ox, c) in coords.iter().enumerate() {
            let Some((c, row)) = *c else { continue };
            if !(c >= 0.0 && row >= 0.0 && c < w && row < h) {
                continue;
            }
            sampler.sample(c, row, &mut px);
            canvas.set(&mut bands, oy * ow + ox, &px);
        }
    }
    canvas.finish(r, g, bands, name)
}

/// `r` on the grid `g`, each cell the mean (or, with `mode`, the most frequent value, with GDAL's
/// tie rule: see [`mode_of`]) of the pixels whose centres fall in it. A cell's corners are placed
/// on the image (`source`) and the pixel centres inside the quadrilateral they make are taken; one
/// with none takes the pixel under its centre. A palette image takes the mode.
fn aggregated(
    r: &Raster,
    g: &Grid,
    canvas: Canvas,
    mode: bool,
    source: &dyn Fn(f64, f64) -> Option<(f64, f64)>,
    name: String,
) -> Result<Raster, String> {
    let (w, h) = (r.width as usize, r.height as usize);
    let (ow, oh) = (g.width, g.height);
    let mode = mode || matches!(r.color, ColorHint::Palette(_));
    let round = !r.data_type.starts_with("Float");
    let nearest = Sampler { r, method: Resampling::Nearest, alpha: None, round };
    let mut bands = canvas.bands(ow, oh)?;
    let mut nodes = Vec::new();
    // Cell corners: a row's top edge, then its bottom edge (the next row's top).
    let x_at = |i: usize| g.min_x + i as f64 * g.cell_x;
    let mut top: Vec<Option<(f64, f64)>> = vec![None; ow + 1];
    let mut bottom: Vec<Option<(f64, f64)>> = vec![None; ow + 1];
    row_positions(&x_at, g.max_y, source, &mut nodes, &mut top);
    let mut values: Vec<Vec<f32>> = vec![Vec::new(); canvas.nb];
    // Each value's weight: a colour counts as much as its pixel is opaque, as bilinear weighs it.
    let mut weights: Vec<Vec<f32>> = vec![Vec::new(); canvas.nb];
    let mut sums: Vec<(f64, f64)> = vec![(0.0, 0.0); canvas.nb];
    let alpha = canvas.alpha_src;
    let mut cell = vec![f32::NAN; canvas.nb];
    for oy in 0..oh {
        row_positions(&x_at, g.max_y - (oy + 1) as f64 * g.cell_y, source, &mut nodes, &mut bottom);
        for ox in 0..ow {
            let corners = (top.get(ox), top.get(ox + 1), bottom.get(ox + 1), bottom.get(ox));
            let (Some(Some(tl)), Some(Some(tr)), Some(Some(br)), Some(Some(bl))) = corners else { continue };
            let quad = [*tl, *tr, *br, *bl];
            let (c0, c1) = quad.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.0), hi.max(p.0)));
            let (r0, r1) = quad.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
            if c1 <= 0.0 || r1 <= 0.0 || c0 >= w as f64 || r0 >= h as f64 {
                continue;
            }
            // The pixels whose centres (at half integers) may be inside.
            let first = |lo: f64| (lo - 0.5).ceil().max(0.0) as usize;
            let past = |hi: f64, n: usize| ((hi - 0.5).floor() + 1.0).clamp(0.0, n as f64) as usize;
            let (i0, i1, j0, j1) = (first(c0), past(c1, w), first(r0), past(r1, h));
            values.iter_mut().for_each(Vec::clear);
            weights.iter_mut().for_each(Vec::clear);
            // The mean is summed as the pixels come (no copy of the cell's values); mode keeps them.
            sums.iter_mut().for_each(|s| *s = (0.0, 0.0));
            let mut inside = false;
            for j in j0..j1 {
                for i in i0..i1 {
                    if !holds(&quad, (i as f64 + 0.5, j as f64 + 0.5)) {
                        continue;
                    }
                    inside = true;
                    let idx = j * w + i;
                    // An alpha with no value (the raster's no-data) or below 0 weighs nothing.
                    let opacity = alpha
                        .and_then(|a| r.bands.get(a))
                        .and_then(|band| band.data.get(idx))
                        .copied()
                        .filter(|a| r.is_valid(*a) && *a > 0.0)
                        .unwrap_or(0.0);
                    for (b, ((v, k), sum)) in values.iter_mut().zip(weights.iter_mut()).zip(sums.iter_mut()).enumerate() {
                        if let Some(x) = r.bands.get(b).and_then(|band| band.data.get(idx)).filter(|x| r.is_valid(**x)) {
                            let weight = if alpha.is_some_and(|a| a != b) { opacity } else { 1.0 };
                            if mode {
                                v.push(*x);
                                k.push(weight);
                            } else {
                                *sum = (sum.0 + f64::from(*x) * f64::from(weight), sum.1 + f64::from(weight));
                            }
                        }
                    }
                }
            }
            let at = oy * ow + ox;
            if inside {
                for (b, (((o, v), k), sum)) in cell.iter_mut().zip(values.iter_mut()).zip(&weights).zip(&sums).enumerate() {
                    *o = if mode {
                        // The most frequent colour, and alpha, among the pixels that show:
                        // see-through ones count for nothing, as in the mean. A cell where none
                        // shows takes the mode of all (see-through, alpha 0).
                        let shows = |(x, k): (&f32, &f32)| if alpha == Some(b) { *x > 0.0 } else { *k > 0.0 };
                        let shown: Vec<f32> = v.iter().zip(k).filter(|p| shows(*p)).map(|(x, _)| *x).collect();
                        match (shown.is_empty(), alpha) {
                            (false, _) => mode_of(&shown),
                            // No pixel that shows has a colour here: none is taken from one that
                            // doesn't.
                            (true, Some(a)) if a != b => f32::NAN,
                            (true, _) => mode_of(v),
                        }
                    } else {
                        mean_of(*sum, round)
                    };
                }
            } else {
                // A grid finer than the raster: the pixel under the cell's centre.
                let (cx, cy) = (quad.iter().map(|p| p.0).sum::<f64>() / 4.0, quad.iter().map(|p| p.1).sum::<f64>() / 4.0);
                if !(cx >= 0.0 && cy >= 0.0 && cx < w as f64 && cy < h as f64) {
                    continue;
                }
                nearest.sample(cx, cy, &mut cell);
            }
            canvas.set(&mut bands, at, &cell);
        }
        std::mem::swap(&mut top, &mut bottom);
    }
    canvas.finish(r, g, bands, name)
}

/// Whether `p` is inside the quadrilateral `q`: an output cell's top left, top right, bottom
/// right and bottom left corners as image positions. Its top and left edges count and its right
/// and bottom ones don't, so a pixel centre on the edge between two cells is in one of them.
fn holds(q: &[(f64, f64); 4], p: (f64, f64)) -> bool {
    let [a, b, c, d] = *q;
    // Twice the signed area: which side of each edge is inside.
    let turn = (a.0 * b.1 - b.0 * a.1) + (b.0 * c.1 - c.0 * b.1) + (c.0 * d.1 - d.0 * c.1) + (d.0 * a.1 - a.0 * d.1);
    if turn == 0.0 || !turn.is_finite() {
        return false;
    }
    let s = turn.signum();
    let side = |from: (f64, f64), to: (f64, f64)| s * ((to.0 - from.0) * (p.1 - from.1) - (to.1 - from.1) * (p.0 - from.0));
    side(a, b) >= 0.0 && side(b, c) > 0.0 && side(c, d) > 0.0 && side(d, a) >= 0.0
}

/// The mean of a cell's values from their weighed sum and total weight (a colour weighs as its
/// pixel's alpha), rounded for a whole-number pixel type; NaN when nothing weighs (no value, or
/// all see-through).
fn mean_of((sum, weight): (f64, f64), round: bool) -> f32 {
    if weight <= 1e-12 {
        return f32::NAN;
    }
    let m = (sum / weight) as f32;
    if round { m.round() } else { m }
}

/// The most frequent value of `v`; NaN when it is empty. Of the values tied, the one that reaches
/// the top count first, `v` taken in its order (the cell's pixels, row by row), as GDAL's `mode`
/// does: a cell of 3, 5, 5, 3 takes 5, one of 5, 3, 3, 5 takes 3.
fn mode_of(v: &[f32]) -> f32 {
    // Capped: a cell can take many pixels, and a few distinct values are the common case.
    let mut counts: HashMap<u32, usize> = HashMap::with_capacity(v.len().min(256));
    let (mut best, mut best_n) = (f32::NAN, 0usize);
    for &x in v {
        // `+ 0.0` counts -0.0 and 0.0 as one value, as `==` does.
        let n = counts.entry((x + 0.0).to_bits()).or_insert(0);
        *n += 1;
        if *n > best_n {
            (best, best_n) = (x, *n);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::georef::{self, Gcp, Kind};

    fn scan(w: u32, h: u32, bands: Vec<Vec<f32>>, color: ColorHint, data_type: &str) -> Raster {
        let gt = GeoTransform { origin_x: 0.0, origin_y: f64::from(h), pixel_width: 1.0, pixel_height: -1.0 };
        Raster::new("scan", w, h, bands, gt, Crs::LonLat, None, color, data_type).unwrap()
    }

    fn opts(cell: Option<f64>, resampling: Resampling) -> Options {
        Options { crs: Crs::from_epsg(32613).unwrap(), points_crs: Crs::from_epsg(32613).unwrap(), resampling, cell, name: "w".into() }
    }

    fn gcps(w: f64, h: f64, f: impl Fn(f64, f64) -> (f64, f64)) -> Vec<Gcp> {
        let mut v = Vec::new();
        for i in 0..4 {
            for j in 0..4 {
                let (col, row) = (w * f64::from(i) / 3.0, h * f64::from(j) / 3.0);
                let (x, y) = f(col, row);
                v.push(Gcp { col, row, x, y, on: true });
            }
        }
        v
    }

    #[test]
    fn a_bent_fit_is_followed_between_exact_steps() {
        // x runs ahead of the column by 0.1 col²: bent enough that the linear steps between
        // exact computations are too far off in places, and the exact way back is used there.
        let (w, h) = (60u32, 40u32);
        let data: Vec<f32> = (0..w * h).map(|i| (i % w) as f32 + 0.5).collect();
        let r = scan(w, h, vec![data], ColorHint::Gray, "Float32");
        let map = |c: f64, rw: f64| (500_000.0 + 10.0 * c + 0.1 * c * c, 4_000_000.0 - 10.0 * rw);
        let fit = georef::fit(Kind::Polynomial2, &gcps(60.0, 40.0, map)).unwrap();
        let out = warp(&r, &fit, &opts(Some(5.0), Resampling::Bilinear)).unwrap();
        let mut checked = 0;
        for row in (0..i64::from(out.height)).step_by(11) {
            for col in 0..i64::from(out.width) {
                let p = out.transform.to_world(col as f64 + 0.5, row as f64 + 0.5);
                // The fit's own way back, computed for this cell alone (polynomial fits are
                // fitted in each direction, so it isn't quite the inverse of the way forward).
                let Some((c, _)) = fit.to_pixel(p.x, p.y) else { continue };
                if (1.0..59.0).contains(&c) {
                    let v = out.value(0, col, row).unwrap();
                    assert!((f64::from(v) - c).abs() <= TOLERANCE + 1e-3, "cell {col},{row}: {v} against {c}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 100);
    }

    #[test]
    fn a_colour_image_gets_an_alpha_band_and_the_outside_is_see_through() {
        let (w, h) = (40u32, 40u32);
        let n = (w * h) as usize;
        let r = scan(w, h, vec![vec![200.0; n], vec![100.0; n], vec![50.0; n]], ColorHint::Rgb { alpha: false }, "UInt8");
        let (sin, cos) = 40f64.to_radians().sin_cos();
        let map = |c: f64, rw: f64| (500_000.0 + c * cos + rw * sin, 4_000_000.0 + c * sin - rw * cos);
        let fit = georef::fit(Kind::Similarity, &gcps(40.0, 40.0, map)).unwrap();
        let out = warp(&r, &fit, &opts(None, Resampling::Bilinear)).unwrap();
        assert_eq!((out.band_count(), out.color.clone()), (4, ColorHint::Rgb { alpha: true }));
        assert_eq!(out.value(3, 0, 0), Some(0.0), "the box's corner is outside the image");
        let (mid_c, mid_r) = (i64::from(out.width) / 2, i64::from(out.height) / 2);
        assert_eq!([0, 1, 2, 3].map(|b| out.value(b, mid_c, mid_r)), [Some(200.0), Some(100.0), Some(50.0), Some(255.0)]);
        // The image's cells keep whole-number values.
        assert!(out.bands.iter().all(|b| b.data.iter().all(|v| v.fract() == 0.0)));
    }

    #[test]
    fn cubic_stays_within_the_bands_range_at_an_edge() {
        let (w, h) = (30u32, 30u32);
        let data: Vec<f32> = (0..w * h).map(|i| if i % w < 15 { 0.0 } else { 100.0 }).collect();
        let r = scan(w, h, vec![data], ColorHint::Gray, "Float32");
        let map = |c: f64, rw: f64| (500_000.0 + 7.0 * c, 4_000_000.0 - 7.0 * rw);
        let fit = georef::fit(Kind::Affine, &gcps(30.0, 30.0, map)).unwrap();
        let out = warp(&r, &fit, &opts(Some(2.0), Resampling::Cubic)).unwrap();
        let (lo, hi) = out.bands[0].data.iter().filter(|v| v.is_finite()).fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        assert!(lo >= 0.0 && hi <= 100.0, "{lo}..{hi}");
        // Nearest keeps only the two values.
        let out = warp(&r, &fit, &opts(Some(2.0), Resampling::Nearest)).unwrap();
        assert!(out.bands[0].data.iter().filter(|v| v.is_finite()).all(|v| *v == 0.0 || *v == 100.0));
    }

    #[test]
    fn cubic_takes_no_colour_from_see_through_pixels() {
        // Opaque red on the left, see-through green on the right.
        let (w, h) = (30u32, 30u32);
        let half = |a: f32, b: f32| (0..w * h).map(|i| if i % w < 15 { a } else { b }).collect::<Vec<f32>>();
        let bands = vec![half(255.0, 0.0), half(0.0, 255.0), half(0.0, 0.0), half(255.0, 0.0)];
        let r = scan(w, h, bands, ColorHint::Rgb { alpha: true }, "UInt8");
        let map = |c: f64, rw: f64| (500_000.0 + 7.0 * c, 4_000_000.0 - 7.0 * rw);
        let fit = georef::fit(Kind::Affine, &gcps(30.0, 30.0, map)).unwrap();
        let out = warp(&r, &fit, &opts(Some(2.0), Resampling::Cubic)).unwrap();
        let mut edge = 0;
        for at in 0..out.bands[3].data.len() {
            let a = out.bands[3].data[at];
            if a > 0.0 {
                let rgb = [0, 1, 2].map(|b| out.bands[b].data[at]);
                assert_eq!(rgb, [255.0, 0.0, 0.0], "cell {at}, alpha {a}");
                edge += usize::from(a < 255.0);
            }
        }
        assert!(edge > 0, "some cells are partly see-through");
    }

    #[test]
    fn cubic_leaves_unequal_alphas_to_bilinear() {
        // Alpha rises across every column, so no sixteen pixels share one, and the colours bend.
        let (w, h) = (30u32, 30u32);
        let band = |f: &dyn Fn(u32, u32) -> f32| (0..w * h).map(|i| f(i % w, i / w)).collect::<Vec<f32>>();
        let bands = vec![
            band(&|x, _| (x * x / 4) as f32),
            band(&|_, y| (255 - 8 * y) as f32),
            band(&|x, y| ((x * 37 + y * 91) % 256) as f32),
            band(&|x, _| (40 + 7 * x) as f32),
        ];
        let r = scan(w, h, bands, ColorHint::Rgb { alpha: true }, "UInt8");
        let map = |c: f64, rw: f64| (500_000.0 + 7.0 * c, 4_000_000.0 - 7.0 * rw);
        let fit = georef::fit(Kind::Affine, &gcps(30.0, 30.0, map)).unwrap();
        let cubic = warp(&r, &fit, &opts(Some(2.0), Resampling::Cubic)).unwrap();
        let bilinear = warp(&r, &fit, &opts(Some(2.0), Resampling::Bilinear)).unwrap();
        for b in 0..3 {
            let (c, l) = (&cubic.bands[b].data, &bilinear.bands[b].data);
            assert_eq!(c.len(), l.len());
            assert!(c.iter().filter(|v| v.is_finite()).count() > 100, "band {b} has cells to compare");
            for (at, (x, y)) in c.iter().zip(l).enumerate() {
                assert!(x == y || (x.is_nan() && y.is_nan()), "band {b}, cell {at}: cubic {x}, bilinear {y}");
            }
        }
    }

    #[test]
    fn no_data_is_kept_outside_and_skipped_inside() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 4.0, pixel_width: 1.0, pixel_height: -1.0 };
        let mut data = vec![10.0f32; 16];
        data[5] = -9999.0;
        let r = Raster::new("nd", 4, 4, vec![data], gt, Crs::LonLat, Some(-9999.0), ColorHint::Gray, "Float32").unwrap();
        let (sin, cos) = 25f64.to_radians().sin_cos();
        let map = |c: f64, rw: f64| (500_000.0 + (c * cos + rw * sin) * 10.0, 4_000_000.0 + (c * sin - rw * cos) * 10.0);
        let fit = georef::fit(Kind::Similarity, &gcps(4.0, 4.0, map)).unwrap();
        let out = warp(&r, &fit, &opts(Some(2.0), Resampling::Bilinear)).unwrap();
        assert_eq!(out.nodata, Some(-9999.0));
        assert_eq!(out.bands[0].data[0], -9999.0, "outside the image: the raster's no-data value");
        // Bilinear skips the no-data pixel: nothing but 10 and no-data comes out.
        assert!(out.bands[0].data.iter().all(|v| *v == 10.0 || *v == -9999.0));
    }

    /// The control-point warp shares the no-data rule of #973: a cell whose centre falls on a
    /// no-data pixel has no value, even with valid pixels around it.
    #[test]
    fn warp_leaves_a_cell_over_no_data_without_a_value() {
        // 6 × 6 pixels of 10 with a 2 × 2 no-data hole at columns and rows 2–3, 1 m square and
        // unrotated; half-metre cells, centred a quarter pixel from the pixel edges.
        let gt = GeoTransform { origin_x: 0.0, origin_y: 6.0, pixel_width: 1.0, pixel_height: -1.0 };
        let mut data = vec![10.0f32; 36];
        for idx in [14, 15, 20, 21] {
            data[idx] = -9999.0;
        }
        let r = Raster::new("nd", 6, 6, vec![data], gt, Crs::LonLat, Some(-9999.0), ColorHint::Gray, "Float32").unwrap();
        let fit = georef::fit(Kind::Similarity, &gcps(6.0, 6.0, |c, rw| (500_000.0 + c, 4_000_000.0 - rw))).unwrap();
        let out = warp(&r, &fit, &opts(Some(0.5), Resampling::Bilinear)).unwrap();
        // 12 × 12 cells over the image (one more column or row if the fit rounds the footprint up).
        assert!(out.width >= 12 && out.height >= 12);
        // The 4 × 4 cells over the hole have no value; before, the outer ring of them took the mean
        // of the valid neighbours and only the middle 2 × 2 were no-data.
        let empty: Vec<(i64, i64)> =
            (0..12).flat_map(|row| (0..12).map(move |c| (c, row))).filter(|&(c, row)| out.value(0, c, row).is_none()).collect();
        let hole: Vec<(i64, i64)> = (4..8).flat_map(|row| (4..8).map(move |c| (c, row))).collect();
        assert_eq!(empty, hole);
        assert!(out.bands[0].data.iter().all(|v| *v == 10.0 || *v == -9999.0));
    }

    #[test]
    fn hostile_fits_and_options_are_refused() {
        let r = scan(4, 4, vec![vec![1.0; 16]], ColorHint::Gray, "Float32");
        let map = |c: f64, rw: f64| (500_000.0 + c, 4_000_000.0 - rw);
        let fit = georef::fit(Kind::Affine, &gcps(4.0, 4.0, map)).unwrap();
        for cell in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-12, 1e-300] {
            assert!(warp(&r, &fit, &opts(Some(cell), Resampling::Nearest)).is_err(), "{cell}");
        }
        // Enormous coordinates: no panic, whatever the answer.
        let huge = |c: f64, rw: f64| (c * 1e300, rw * 1e300);
        if let Ok(fit) = georef::fit(Kind::Affine, &gcps(4.0, 4.0, huge)) {
            let _ = warp(&r, &fit, &opts(None, Resampling::Cubic));
            let _ = warp(&r, &fit, &opts(Some(1.0), Resampling::Cubic));
        }
    }

    #[test]
    fn a_grey_byte_image_becomes_grey_with_alpha_and_every_type_is_opaque_at_255() {
        let (sin, cos) = 30f64.to_radians().sin_cos();
        let map = |c: f64, rw: f64| (500_000.0 + (c * cos + rw * sin) * 10.0, 4_000_000.0 + (c * sin - rw * cos) * 10.0);
        let fit = georef::fit(Kind::Similarity, &gcps(20.0, 20.0, map)).unwrap();
        let r = scan(20, 20, vec![vec![90.0; 400]], ColorHint::Gray, "UInt8");
        let out = warp(&r, &fit, &opts(None, Resampling::Bilinear)).unwrap();
        assert_eq!((out.band_count(), out.color.clone()), (2, ColorHint::GrayAlpha));
        let (c, rw) = (i64::from(out.width) / 2, i64::from(out.height) / 2);
        assert_eq!((out.value(0, c, rw), out.value(1, c, rw), out.value(1, 0, 0)), (Some(90.0), Some(255.0), Some(0.0)));
        // A 16-bit colour image: alpha is 255 too, as the renderer reads it.
        let n = 400;
        let r = scan(20, 20, vec![vec![60_000.0; n], vec![100.0; n], vec![7.0; n]], ColorHint::Rgb { alpha: false }, "UInt16");
        let out = warp(&r, &fit, &opts(None, Resampling::Bilinear)).unwrap();
        assert_eq!(out.value(3, c, rw), Some(255.0));
    }

    /// A `w` × `h` raster in UTM 13N with 1 m pixels, its top left corner at (0, `h`), no-data -1.
    fn utm(w: u32, h: u32, values: Vec<f32>, data_type: &str) -> Raster {
        let gt = GeoTransform { origin_x: 0.0, origin_y: f64::from(h), pixel_width: 1.0, pixel_height: -1.0 };
        let crs = Crs::from_epsg(32613).unwrap();
        Raster::new("r", w, h, vec![values], gt, crs, Some(-1.0), ColorHint::Gray, data_type).unwrap()
    }

    fn values(r: &Raster) -> Vec<Option<f32>> {
        (0..i64::from(r.height)).flat_map(|row| (0..i64::from(r.width)).map(move |c| (c, row))).map(|(c, row)| r.value(0, c, row)).collect()
    }

    /// Bilinear and cubic leave a cell no-data where the pixel under its centre is no-data, and
    /// otherwise average the valid neighbours, as gdalwarp does (#973).
    #[test]
    fn bilinear_and_cubic_leave_a_cell_over_no_data_without_a_value() {
        // 6 × 6 pixels, value = column + 10 · row, with a 2 × 2 no-data hole at columns and rows
        // 2–3; cells shifted a quarter pixel, so cell (k, r) is centred in pixel (k, r + 1).
        let mut v: Vec<f32> = (0..36).map(|i| (i % 6 + 10 * (i / 6)) as f32).collect();
        for idx in [14, 15, 20, 21] {
            v[idx] = -1.0;
        }
        // The hole as the no-data value, and as NaN in a raster without one.
        let nan: Vec<f32> = v.iter().map(|&x| if x < 0.0 { f32::NAN } else { x }).collect();
        let r = utm(6, 6, v, "Float32");
        let mut no_nodata = utm(6, 6, nan, "Float32");
        no_nodata.nodata = None;
        let g = Grid::snapped(r.crs, [0.25, 0.25, 5.25, 5.25], (1.0, 1.0), (0.25, 0.25)).unwrap();
        assert_eq!((g.width, g.height), (5, 5));
        // gdalwarp -r bilinear and -r cubic (GDAL 3.12.2 and 3.13.1) on the same grid; the NaN
        // raster with `-srcnodata nan` gives the same values.
        let n = None;
        let gdal = [
            [Some(7.75), Some(8.75), Some(9.75), Some(10.75), Some(11.75)],
            [Some(17.75), Some(18.0), n, n, Some(21.75)],
            [Some(27.75), Some(28.5), n, n, Some(31.75)],
            [Some(37.75), Some(39.2), Some(42.25), Some(42.538_46), Some(41.75)],
            [Some(47.75), Some(48.75), Some(49.75), Some(50.75), Some(51.75)],
        ];
        for src in [&r, &no_nodata] {
            let out = values(&project(src, &g, Method::Sample(Resampling::Bilinear), "b".into()).unwrap());
            for (got, want) in out.iter().zip(gdal.iter().flatten()) {
                assert_eq!(got.is_some(), want.is_some(), "{out:?}");
                if let (Some(a), Some(b)) = (got, want) {
                    assert!((a - b).abs() < 1e-4, "{out:?}");
                }
            }
            // Cubic: the cells over the hole have no value either. The others in columns 1–4 of
            // rows 1–3 have a hole pixel among their sixteen, so they fall back to bilinear, as in
            // GDAL; this test doesn't reach the cubic kernel itself.
            let out = values(&project(src, &g, Method::Sample(Resampling::Cubic), "c".into()).unwrap());
            for (i, (got, want)) in out.iter().zip(gdal.iter().flatten()).enumerate() {
                assert_eq!(got.is_some(), want.is_some(), "{out:?}");
                let (row, col) = (i / 5, i % 5);
                if (1..=3).contains(&row)
                    && (1..=4).contains(&col)
                    && let (Some(a), Some(b)) = (got, want)
                {
                    assert!((a - b).abs() < 1e-4, "cell ({col}, {row}): {out:?}");
                }
            }
        }
    }

    /// A colour pixel has no value only where every channel is no-data: one channel at the
    /// no-data value blends from the neighbours, as in gdalwarp (`UNIFIED_SRC_NODATA=PARTIAL`).
    #[test]
    fn a_colour_cell_needs_every_channel_no_data_to_have_no_value() {
        // 6 × 6 pixels of (100, 150, 200), no-data 0: pixel (2, 2) has red 0, pixel (4, 4) is 0 in
        // every channel. Cells shifted a quarter pixel, so cell (k, r) is centred in pixel (k, r + 1).
        let mut bands = vec![vec![100.0f32; 36], vec![150.0; 36], vec![200.0; 36]];
        bands[0][14] = 0.0;
        for band in &mut bands {
            band[28] = 0.0;
        }
        let gt = GeoTransform { origin_x: 0.0, origin_y: 6.0, pixel_width: 1.0, pixel_height: -1.0 };
        let crs = Crs::from_epsg(32613).unwrap();
        let r = Raster::new("rgb", 6, 6, bands, gt, crs, Some(0.0), ColorHint::Rgb { alpha: false }, "UInt8").unwrap();
        let g = Grid::snapped(r.crs, [0.25, 0.25, 5.25, 5.25], (1.0, 1.0), (0.25, 0.25)).unwrap();
        // gdalwarp -r bilinear and -r cubic (GDAL 3.12.2 and 3.13.1): (100, 150, 200) over the
        // pixel with red 0, and no value over the one that is 0 in every channel.
        for method in [Resampling::Bilinear, Resampling::Cubic] {
            let out = project(&r, &g, Method::Sample(method), "o".into()).unwrap();
            let cell = |c, row| (0..3).map(|b| out.value(b, c, row)).collect::<Vec<_>>();
            assert_eq!(cell(2, 1), [Some(100.0), Some(150.0), Some(200.0)], "{method:?}");
            assert_eq!(cell(4, 3), [None, None, None], "{method:?}");
        }
    }

    /// A colour image without no-data gains an alpha band; a cell over a pixel with no value in
    /// any band (NaN in a float image) is see-through there, not opaque black.
    #[test]
    fn an_added_alpha_is_clear_over_a_pixel_with_no_value() {
        // 6 × 6 pixels of (100, 150, 200), Float32 without no-data, pixel (2, 2) NaN in every
        // band. Cells shifted a quarter pixel, so cell (k, r) is centred in pixel (k, r + 1).
        let mut bands = vec![vec![100.0f32; 36], vec![150.0; 36], vec![200.0; 36]];
        for band in &mut bands {
            band[14] = f32::NAN;
        }
        let gt = GeoTransform { origin_x: 0.0, origin_y: 6.0, pixel_width: 1.0, pixel_height: -1.0 };
        let crs = Crs::from_epsg(32613).unwrap();
        let r = Raster::new("rgb", 6, 6, bands, gt, crs, None, ColorHint::Rgb { alpha: false }, "Float32").unwrap();
        let g = Grid::snapped(r.crs, [0.25, 0.25, 5.25, 5.25], (1.0, 1.0), (0.25, 0.25)).unwrap();
        for method in [Resampling::Nearest, Resampling::Bilinear, Resampling::Cubic] {
            let out = project(&r, &g, Method::Sample(method), "o".into()).unwrap();
            assert_eq!(out.bands.len(), 4, "{method:?}: an alpha band is added");
            let alpha = |c: usize, row: usize| out.bands[3].data[row * 5 + c];
            assert_eq!(alpha(2, 1), 0.0, "{method:?}: over the NaN pixel");
            assert_eq!(alpha(1, 1), 255.0, "{method:?}: next to it");
        }
    }

    /// Average and mode take every pixel whose centre falls in a cell, no-data skipped; mode's
    /// ties go to the value that reaches the top count first, row by row, as in GDAL.
    #[test]
    fn average_and_mode_take_every_pixel_in_a_cell() {
        // 4 × 4 pixels, value = column + 10 · row; pixel (1, 0) is no-data.
        let mut v: Vec<f32> = (0..16).map(|i| (i % 4 + 10 * (i / 4)) as f32).collect();
        v[1] = -1.0;
        let r = utm(4, 4, v, "Float32");
        let g = Grid::snapped(r.crs, [0.0, 0.0, 4.0, 4.0], (2.0, 2.0), (0.0, 0.0)).unwrap();
        assert_eq!((g.width, g.height), (2, 2));
        let avg = project(&r, &g, Method::Average, "a".into()).unwrap();
        // Top left: 0, 10 and 11 (1 is no-data); top right: 2, 3, 12, 13.
        assert_eq!(values(&avg), vec![Some(7.0), Some(7.5), Some(25.5), Some(27.5)]);
        // Classes: a cell of 3, 5, 5, 3 ties and takes 5, which reaches two first; one of 7, 7, 7, 2
        // takes 7. GDAL 3.12.2 and 3.13.1 (`gdalwarp -r mode -tr 2 2`) give 5, 7, 1, 4.
        let mut classes = vec![3.0, 5.0, 7.0, 7.0, 5.0, 3.0, 7.0, 2.0, 1.0, 1.0, 4.0, 4.0, 1.0, 9.0, 4.0, 4.0];
        let r = utm(4, 4, classes.clone(), "UInt8");
        let mode = project(&r, &g, Method::Mode, "m".into()).unwrap();
        assert_eq!(values(&mode), vec![Some(5.0), Some(7.0), Some(1.0), Some(4.0)]);
        // The order decides, not the value: 5, 3, 3, 5 takes 3 (GDAL too).
        classes[..2].copy_from_slice(&[5.0, 3.0]);
        classes[4..6].copy_from_slice(&[3.0, 5.0]);
        let mirrored = project(&utm(4, 4, classes, "UInt8"), &g, Method::Mode, "m".into()).unwrap();
        assert_eq!(values(&mirrored)[0], Some(3.0));
        // -0.0 and 0.0 are one value: the zero reaches two first, the 1 before it doesn't.
        assert_eq!(mode_of(&[1.0, -0.0, 0.0]), 0.0);
        // A whole-number type's mean is rounded: (3 + 5 + 5 + 3) / 4 = 4, (7 + 7 + 7 + 2) / 4 = 5.75.
        let avg = project(&r, &g, Method::Average, "a".into()).unwrap();
        assert_eq!(values(&avg)[..2], [Some(4.0), Some(6.0)]);
    }

    /// Average weighs a colour image's colours by alpha, as bilinear does: a see-through pixel
    /// takes no colour into its cell; the alpha itself is the plain mean.
    #[test]
    fn average_weighs_colours_by_alpha() {
        // 2 × 2 pixels into one cell: opaque red on the left, see-through black on the right.
        let gt = GeoTransform { origin_x: 0.0, origin_y: 2.0, pixel_width: 1.0, pixel_height: -1.0 };
        let half = |a: f32, b: f32| vec![a, b, a, b];
        let bands = vec![half(255.0, 0.0), half(0.0, 0.0), half(0.0, 0.0), half(255.0, 0.0)];
        let crs = Crs::from_epsg(32613).unwrap();
        let r = Raster::new("rgba", 2, 2, bands, gt, crs, None, ColorHint::Rgb { alpha: true }, "UInt8").unwrap();
        let g = Grid::snapped(r.crs, [0.0, 0.0, 2.0, 2.0], (2.0, 2.0), (0.0, 0.0)).unwrap();
        let out = project(&r, &g, Method::Average, "a".into()).unwrap();
        assert_eq!([0, 1, 2, 3].map(|b| out.value(b, 0, 0)), [Some(255.0), Some(0.0), Some(0.0), Some(128.0)]);
        // Mode counts the pixels that show: three see-through black ones lose to one opaque red.
        let wide = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
        let px = |a: f32, b: f32| vec![a, b, b, b];
        let bands = vec![px(255.0, 0.0), px(0.0, 0.0), px(0.0, 0.0), px(255.0, 0.0)];
        let r = Raster::new("rgba", 4, 1, bands, wide, crs, None, ColorHint::Rgb { alpha: true }, "UInt8").unwrap();
        let g = Grid::snapped(r.crs, [0.0, 0.0, 4.0, 1.0], (4.0, 1.0), (0.0, 0.0)).unwrap();
        let out = project(&r, &g, Method::Mode, "m".into()).unwrap();
        assert_eq!([0, 1, 2, 3].map(|b| out.value(b, 0, 0)), [Some(255.0), Some(0.0), Some(0.0), Some(255.0)], "red, and showing");
        // The opaque pixel has no red: the see-through pixels' red isn't taken instead.
        let bands = vec![px(-1.0, 90.0), px(0.0, 0.0), px(0.0, 0.0), px(255.0, 0.0)];
        let r = Raster::new("rgba", 4, 1, bands, wide, crs, Some(-1.0), ColorHint::Rgb { alpha: true }, "Int16").unwrap();
        let out = project(&r, &g, Method::Mode, "m".into()).unwrap();
        // (A cell without a value is cleared to 0 in an image with alpha, as outside the raster.)
        assert_eq!([0, 3].map(|b| out.value(b, 0, 0)), [Some(0.0), Some(255.0)], "not the see-through 90");
    }

    /// A pixel centre on the edge between cells is in exactly one of them, and a grid finer than
    /// the raster takes the pixel under each cell's centre.
    #[test]
    fn each_pixel_is_in_one_cell_and_a_finer_grid_takes_the_nearest() {
        // Value = column + 10 · row.
        let r = utm(4, 4, (0..16).map(|i| (i % 4 + 10 * (i / 4)) as f32).collect(), "Float32");
        // Cells of 2 whose edges run through pixel centres: a centre on a cell's top or left edge
        // is in it, one on its right or bottom edge in the next, so each is taken once.
        let g = Grid::snapped(r.crs, [0.0, 0.0, 4.0, 4.0], (2.0, 2.0), (0.5, 0.5)).unwrap();
        assert_eq!((g.width, g.height, g.min_x, g.max_y), (3, 3, -1.5, 4.5));
        let out = project(&r, &g, Method::Average, "a".into()).unwrap();
        let expected = [None, Some(0.5), Some(2.5), None, Some(15.5), Some(17.5), None, Some(30.5), Some(32.5)];
        assert_eq!(values(&out), expected);
        // Cells of 0.5: each takes the pixel it is in.
        let g = Grid::snapped(r.crs, [0.0, 0.0, 4.0, 4.0], (0.5, 0.5), (0.0, 0.0)).unwrap();
        let out = project(&r, &g, Method::Average, "a".into()).unwrap();
        assert_eq!((out.width, out.height), (8, 8));
        assert_eq!((out.value(0, 0, 0), out.value(0, 3, 1), out.value(0, 7, 7)), (Some(0.0), Some(1.0), Some(33.0)));
    }

    /// Projected data keeps its bands: a grey byte raster with no no-data value gains no alpha
    /// band (a scan being georeferenced does), and its cells off the raster are 0, as GDAL leaves
    /// them; a floating-point one's are NaN, which is no value.
    #[test]
    fn projected_data_keeps_its_bands() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 4.0, pixel_width: 1.0, pixel_height: -1.0 };
        let crs = Crs::from_epsg(32613).unwrap();
        let g = Grid::snapped(crs, [-2.0, 0.0, 4.0, 4.0], (1.0, 1.0), (0.0, 0.0)).unwrap();
        let classes = Raster::new("c", 4, 4, vec![vec![3.0; 16]], gt, crs, None, ColorHint::Gray, "UInt8").unwrap();
        let out = project(&classes, &g, Method::Mode, "m".into()).unwrap();
        assert_eq!((out.band_count(), out.color.clone(), out.nodata), (1, ColorHint::Gray, None));
        assert_eq!((out.value(0, 0, 0), out.value(0, 2, 0)), (Some(0.0), Some(3.0)));
        let floats = Raster::new("f", 4, 4, vec![vec![3.5; 16]], gt, crs, None, ColorHint::Gray, "Float32").unwrap();
        let out = project(&floats, &g, Method::Sample(Resampling::Bilinear), "b".into()).unwrap();
        assert_eq!((out.band_count(), out.value(0, 0, 0), out.value(0, 3, 1)), (1, None, Some(3.5)));
    }

    /// Grids with the same cell and origin line up wherever their areas are; a grid too large to
    /// hold is refused before anything is made.
    #[test]
    fn snapped_grids_line_up_and_are_bounded() {
        let crs = Crs::from_epsg(31983).unwrap();
        let a = Grid::snapped(crs, [5_123_456.7, 9_000_001.0, 5_200_000.0, 9_100_000.0], (100.0, 100.0), (0.0, 0.0)).unwrap();
        let b = Grid::snapped(crs, [5_150_050.0, 9_050_049.0, 5_300_000.0, 9_200_000.0], (100.0, 100.0), (0.0, 0.0)).unwrap();
        for g in [a, b] {
            assert_eq!((g.min_x % 100.0, g.max_y % 100.0), (0.0, 0.0));
        }
        assert!(a.min_x <= 5_123_456.7 && a.min_x + a.width as f64 * 100.0 >= 5_200_000.0);
        let e = Grid::snapped(crs, [0.0, 0.0, 1e6, 1e6], (0.01, 0.01), (0.0, 0.0)).unwrap_err();
        assert!(e.contains("give a larger cellSize"), "{e}");
        for bad in [(0.0, 1.0), (-1.0, 1.0), (f64::NAN, 1.0), (1.0, f64::INFINITY)] {
            assert!(Grid::snapped(crs, [0.0, 0.0, 10.0, 10.0], bad, (0.0, 0.0)).is_err(), "{bad:?}");
        }
        assert!(Grid::snapped(crs, [10.0, 0.0, 0.0, 10.0], (1.0, 1.0), (0.0, 0.0)).is_err());
        // An origin so far away that the snapped corners overflow is refused, not a 1 × 1 grid.
        let far = Grid::snapped(crs, [-1e308, 0.0, 0.0, 10.0], (1e300, 1.0), (1e308, 0.0)).unwrap_err();
        assert!(far.contains("can't make a grid"), "{far}");
        assert!(Grid::snapped(crs, [0.0, 0.0, 10.0, 10.0], (1.0, 1.0), (f64::NAN, 0.0)).is_err());
    }

    /// A raster projected to another system and back keeps its values in place, within a cell.
    #[test]
    fn a_projection_and_back_keeps_values_in_place() {
        // 40 × 40 pixels of 0.001° near Teresina, value = column + 100 · row.
        let gt = GeoTransform { origin_x: -42.82, origin_y: -5.08, pixel_width: 0.001, pixel_height: -0.001 };
        let v: Vec<f32> = (0..1600).map(|i| (i % 40 + 100 * (i / 40)) as f32).collect();
        let r = Raster::new("t", 40, 40, vec![v], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let utm23s = Crs::from_epsg(31983).unwrap();
        let (bounds, cell) = footprint_in(&r, utm23s).unwrap();
        assert!((cell - 110.0).abs() < 2.0, "{cell}");
        let g = Grid::snapped(utm23s, bounds, (cell, cell), (0.0, 0.0)).unwrap();
        let there = project(&r, &g, Method::Sample(Resampling::Nearest), "p".into()).unwrap();
        let back = project(&there, &Grid::of(&r), Method::Sample(Resampling::Nearest), "b".into()).unwrap();
        let (mut same, mut near) = (0, 0);
        for (a, b) in values(&r).iter().zip(values(&back)) {
            let (Some(a), Some(b)) = (a, b) else { continue };
            // Within a pixel each way: a column of 1, a row of 100.
            let (dc, dr) = ((a % 100.0 - b % 100.0).abs(), ((a / 100.0).floor() - (b / 100.0).floor()).abs());
            same += usize::from(a == &b);
            near += usize::from(dc <= 1.0 && dr <= 1.0);
        }
        assert_eq!(near, 1600, "every pixel within a pixel of its place");
        assert!(same > 1000, "{same} of 1600 in the very same pixel");
        // A longitude/latitude box placed in the projection covers its corners.
        let b = bounds_in([-42.82, -5.12, -42.78, -5.08], utm23s).unwrap();
        assert!((b[0] - bounds[0]).abs() < 1.0 && (b[3] - bounds[3]).abs() < 1.0, "{b:?} {bounds:?}");
    }
}
