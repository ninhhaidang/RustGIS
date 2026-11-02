//! Web Mercator map tiles of a layer whose service draws in a system of its own (#769): each
//! made from the pictures [`Warp::plan`] names (one GetMap of the tile's area, or the tiles of a
//! WMTS matrix covering it), warped into place once they are in the cache, and kept there as the
//! map tile. A layer whose pictures are in Web Mercator already is its service's address as it is.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rustgis_data::ogc::warp::{GRID, MARK, Plan, Warp};
use rustgis_data::raster::Transformer;
use tiny_skia::Pixmap;

use crate::cache::{self, Payload, State, Want};
use crate::template::TileId;

/// Pixels a side of a map tile made here.
const SIZE: u32 = 256;

/// Whether an address is a map tile of such a layer.
pub(crate) fn is_warp(url: &str) -> bool {
    url.contains(MARK)
}

/// For a layer whose pictures are in Web Mercator, its service's address for the tile (filled
/// in already); `None` for one warped (or any other address).
pub(crate) fn direct(url: &str) -> Option<&str> {
    let (base, spec) = url.split_once(MARK)?;
    spec.split(';').any(|f| f == "crs=3857").then_some(base)
}

/// The transformer of a system, made once (they are read from a table of definitions).
pub fn transformer(epsg: u16) -> Option<Arc<Transformer>> {
    rustgis_data::ogc::warp::usable(u32::from(epsg))?;
    rustgis_data::ogc::warp::transformer(u32::from(epsg))
}

/// How long a map tile made without a picture that failed is kept, so it isn't made again for
/// every frame drawn meanwhile: well short of the cache's [`cache::RETRY`], so a picture that
/// answers when asked again fills its hole soon after.
const FOR_NOW: web_time::Duration = web_time::Duration::from_secs(10);
/// The most such tiles kept (a 256 × 256 tile is 256 KiB: at most 64 MiB of them); past it
/// they are all let go.
const MAX_FOR_NOW: usize = 256;

/// Forgets the tiles made without a picture that failed, and the plans kept: the pictures are
/// asked for again ([`cache::clear`], after a login).
pub(crate) fn forget() {
    made_for_now().lock().unwrap_or_else(|e| e.into_inner()).clear();
    plans().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// The map tiles made without a picture that failed: when each was made, and it.
fn made_for_now() -> &'static Mutex<HashMap<String, (web_time::Instant, Arc<Payload>)>> {
    static KEPT: OnceLock<Mutex<HashMap<String, (web_time::Instant, Arc<Payload>)>>> = OnceLock::new();
    KEPT.get_or_init(Mutex::default)
}

/// Map tiles whose pictures are still on their way, and their plans: worked out once, not for
/// every frame drawn meanwhile.
const MAX_PLANS: usize = 4096;

fn plans() -> &'static Mutex<HashMap<String, Arc<Plan>>> {
    static PLANS: OnceLock<Mutex<HashMap<String, Arc<Plan>>>> = OnceLock::new();
    PLANS.get_or_init(Mutex::default)
}

/// What [`make`] made of a map tile.
pub(crate) enum Made {
    /// Its pictures are on their way.
    Waiting,
    /// Made, to keep.
    Done(Payload),
    /// Made without a picture that failed (the service may answer when asked again): shown, not
    /// kept in the cache, and made again once the picture is asked for again ([`FOR_NOW`]).
    ForNow(Arc<Payload>),
    Failed(String),
}

/// The map tile `url` names, made from its pictures when they are all in the cache (asking for
/// those that aren't). A picture the service doesn't have (a WMTS tile past its matrix, see
/// [`missing`]) is left clear, and one that failed otherwise too, the tile made again once it is
/// asked for again; with none there at all, the tile is empty when they were all missing, else
/// it fails.
pub(crate) fn make(url: &str) -> Made {
    if let Some((at, tile)) = made_for_now().lock().unwrap_or_else(|e| e.into_inner()).get(url)
        && at.elapsed() < FOR_NOW
    {
        return Made::ForNow(tile.clone());
    }
    let known = plans().lock().unwrap_or_else(|e| e.into_inner()).get(url).cloned();
    let plan = match known {
        Some(p) => p,
        None => {
            let Some((base, (z, x, y), w)) = Warp::parse(url) else { return Made::Failed("a map tile address that can't be read".into()) };
            let Some(t) = transformer(w.epsg) else {
                return Made::Failed(format!("the coordinate system EPSG:{} isn't supported", w.epsg));
            };
            let e = TileId { z, x, y }.extent();
            Arc::new(w.plan(base, [e.min_x, e.min_y, e.max_x, e.max_y], SIZE, &t))
        }
    };
    if plan.parts.is_empty() {
        return Made::Done(Payload::Empty);
    }
    let mut pictures = Vec::with_capacity(plan.parts.len());
    let (mut waiting, mut error) = (false, None);
    for p in &plan.parts {
        // A matrix tile past the service's coverage is routine; a WMS picture (any area asked) isn't.
        let state = if p.tile.is_some() { cache::get_tile(&p.url, Want::Image) } else { cache::get(&p.url, Want::Image) };
        match state {
            State::Ready(payload) => pictures.push(Some(payload)),
            State::Pending => waiting = true,
            State::Failed(e) => {
                // A WMTS tile the service doesn't have (past its coverage) is a gap, left clear for
                // good; any other failure, and a WMS's 4xx (it draws any area asked), is one the
                // map tile isn't kept with: it is made again once the picture is asked again. (As
                // for any raster tile, the map's layer list doesn't name it: a layer drawn in its
                // own system does, see `native_tiles`.)
                if p.tile.is_none() || !missing(&e) {
                    error = Some(e);
                }
                pictures.push(None);
            }
        }
    }
    // A picture that failed leaves a hole: the tile isn't kept with it, and is made again once
    // the picture is asked for again. (With none there, see below.)
    let for_now = error.is_some() && pictures.iter().any(Option::is_some);
    // Kept while the tile is made again: its pictures on their way, or one failed.
    let mut known = plans().lock().unwrap_or_else(|e| e.into_inner());
    if waiting || for_now {
        if known.len() >= MAX_PLANS {
            known.clear();
        }
        known.insert(url.to_string(), plan.clone());
    } else {
        known.remove(url);
    }
    drop(known);
    if waiting {
        return Made::Waiting;
    }
    if pictures.iter().all(Option::is_none) {
        // None there at all (past the service's coverage): an empty tile, kept.
        return match error {
            Some(e) => Made::Failed(e),
            None => Made::Done(Payload::Empty),
        };
    }
    let tile = compose(&plan, &pictures, SIZE).map_or(Payload::Empty, Payload::Image);
    let mut kept = made_for_now().lock().unwrap_or_else(|e| e.into_inner());
    if !for_now {
        kept.remove(url);
        return Made::Done(tile);
    }
    // Those past their while first, then all if it is still full.
    kept.retain(|_, (at, _)| at.elapsed() < FOR_NOW);
    if kept.len() >= MAX_FOR_NOW {
        kept.clear();
    }
    let tile = Arc::new(tile);
    kept.insert(url.to_string(), (web_time::Instant::now(), tile.clone()));
    Made::ForNow(tile)
}

/// Whether a WMTS tile's failure means the service doesn't have it (past its coverage): a 4xx
/// other than a refused login (401, 403), a timeout (408) or too many requests (429). Such a
/// tile is left clear for good; a WMS picture's 4xx is a failure, asked again (it draws any
/// area asked).
pub fn missing(error: &str) -> bool {
    cache::http_status(error).is_some_and(|s| (400..500).contains(&s) && !matches!(s, 401 | 403 | 408 | 429))
}

/// The map tile, `size` pixels a side, from the plan's pictures (`pictures[i]` is its part `i`'s):
/// each pixel's place in the layer's system interpolated from the plan's grid, and the picture
/// there sampled between its four nearest pixels.
pub(crate) fn compose(plan: &Plan, pictures: &[Option<Arc<Payload>>], size: u32) -> Option<Pixmap> {
    let mut out = Pixmap::new(size, size)?;
    let side = GRID + 1;
    let images: Vec<Option<&Pixmap>> = pictures
        .iter()
        .map(|p| match p.as_deref() {
            Some(Payload::Image(pm)) => Some(pm),
            _ => None,
        })
        .collect();
    let by_tile: HashMap<(u32, u32), usize> = plan.parts.iter().enumerate().filter_map(|(i, p)| Some((p.tile?, i))).collect();
    let n = f64::from(size);
    let data = out.data_mut();
    for py in 0..size {
        let gy = (f64::from(py) + 0.5) / n * GRID as f64;
        let j = (gy.floor() as usize).min(GRID - 1);
        let fy = gy - j as f64;
        for px in 0..size {
            let gx = (f64::from(px) + 0.5) / n * GRID as f64;
            let i = (gx.floor() as usize).min(GRID - 1);
            let fx = gx - i as f64;
            let at = |a: usize, b: usize| plan.points.get(b * side + a).copied().flatten();
            let (Some(p00), Some(p10), Some(p01), Some(p11)) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1)) else { continue };
            let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
            let nx = lerp(lerp(p00.0, p10.0, fx), lerp(p01.0, p11.0, fx), fy);
            let ny = lerp(lerp(p00.1, p10.1, fx), lerp(p01.1, p11.1, fx), fy);
            // The picture there: the GetMap's, or the matrix tile's.
            let part = match &plan.matrix {
                None => Some(0),
                Some(m) => {
                    let col = ((nx - m.left) / (m.cell * f64::from(m.tile_w))).floor();
                    let row = ((m.top - ny) / (m.cell * f64::from(m.tile_h))).floor();
                    (col >= 0.0 && row >= 0.0).then(|| by_tile.get(&(col as u32, row as u32)).copied()).flatten()
                }
            };
            let Some(k) = part else { continue };
            let (Some(Some(img)), Some(p)) = (images.get(k), plan.parts.get(k)) else { continue };
            let [aw, as_, ae, an] = p.area;
            let u = (nx - aw) / (ae - aw) * f64::from(img.width());
            let v = (an - ny) / (an - as_) * f64::from(img.height());
            if let Some(c) = sample(img, u, v) {
                let o = ((py * size + px) * 4) as usize;
                if let Some(d) = data.get_mut(o..o + 4) {
                    d.copy_from_slice(&c);
                }
            }
        }
    }
    Some(out)
}

/// A picture's (premultiplied) colour at (`u`, `v`) in its pixels, between its four nearest
/// pixels; `None` outside it.
fn sample(img: &Pixmap, u: f64, v: f64) -> Option<[u8; 4]> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    if !(u >= 0.0 && v >= 0.0 && u <= w as f64 && v <= h as f64) || w == 0 || h == 0 {
        return None;
    }
    let (x, y) = (u - 0.5, v - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let data = img.data();
    let px = |a: i64, b: i64| -> [f64; 4] {
        let (a, b) = (a.clamp(0, w - 1), b.clamp(0, h - 1));
        let o = ((b * w + a) * 4) as usize;
        let Some(&[r, g, b, a]) = data.get(o..o + 4) else { return [0.0; 4] };
        [f64::from(r), f64::from(g), f64::from(b), f64::from(a)]
    };
    let (x0, y0) = (x0 as i64, y0 as i64);
    let (c00, c10, c01, c11) = (px(x0, y0), px(x0 + 1, y0), px(x0, y0 + 1), px(x0 + 1, y0 + 1));
    let mut out = [0u8; 4];
    for (k, o) in out.iter_mut().enumerate() {
        let top = c00[k] + (c10[k] - c00[k]) * fx;
        let bottom = c01[k] + (c11[k] - c01[k]) * fx;
        *o = (top + (bottom - top) * fy).round().clamp(0.0, 255.0) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_data::ogc::warp::{Matrix, Part};

    #[test]
    fn pictures_a_service_does_not_have() {
        assert!(missing("http status: 404") && !missing("http status: 429") && !missing("http status: 503") && !missing("not an image"));
        assert!(!missing("http status: 401") && !missing("http status: 403"), "a refused login is an error, not a gap");
    }

    #[test]
    fn addresses() {
        let wms = "https://m.example.org/wms?CRS=EPSG:3857&BBOX=1,2,3,4#rustgis-warp=3/1/2;crs=3857;sys=2056|e|EPSG:2056;map=east";
        assert!(is_warp(wms));
        assert_eq!(direct(wms), Some("https://m.example.org/wms?CRS=EPSG:3857&BBOX=1,2,3,4"));
        let warped = "https://m.example.org/wms?CRS=EPSG:2056#rustgis-warp=3/1/2;crs=2056;map=east";
        assert!(is_warp(warped) && direct(warped).is_none());
        assert!(!is_warp("https://t.example.org/3/1/2.png") && direct("https://t.example.org/3/1/2.png").is_none());
    }

    /// Two matrix tiles side by side, red and blue, under a map tile whose grid maps it straight
    /// onto them: its left half red, its right half blue; a grid point outside leaves it clear.
    #[test]
    fn composes_from_the_parts() {
        let fill = |c: tiny_skia::Color| {
            let mut p = Pixmap::new(4, 4).unwrap();
            p.fill(c);
            Some(Arc::new(Payload::Image(p)))
        };
        let m = Matrix { id: "0".into(), left: 0.0, top: 4.0, cell: 1.0, tile_w: 4, tile_h: 4, cols: 2, rows: 1 };
        let side = GRID + 1;
        // The map tile spans x 0..8, y 0..4 of the system.
        let mut points: Vec<Option<(f64, f64)>> =
            (0..side * side).map(|k| Some(((k % side) as f64 / GRID as f64 * 8.0, 4.0 - (k / side) as f64 / GRID as f64 * 4.0))).collect();
        let plan = Plan {
            points: points.clone(),
            parts: vec![
                Part { url: "a".into(), area: [0.0, 0.0, 4.0, 4.0], tile: Some((0, 0)) },
                Part { url: "b".into(), area: [4.0, 0.0, 8.0, 4.0], tile: Some((1, 0)) },
            ],
            matrix: Some(m),
        };
        let pictures = [fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255)), fill(tiny_skia::Color::from_rgba8(0, 0, 255, 255))];
        let out = compose(&plan, &pictures, 16).unwrap();
        let at = |x: u32, y: u32| out.pixel(x, y).map(|p| (p.red(), p.blue(), p.alpha()));
        assert_eq!(at(2, 8), Some((255, 0, 255)));
        assert_eq!(at(13, 8), Some((0, 255, 255)));
        // A picture that failed leaves its part clear.
        let out = compose(&plan, &[pictures[0].clone(), None], 16).unwrap();
        assert_eq!(out.pixel(13, 8).map(|p| p.alpha()), Some(0));
        // Outside the layer (no place in its system): clear.
        points[0] = None;
        let plan = Plan { points, ..plan };
        let out = compose(&plan, &pictures, 16).unwrap();
        assert_eq!(out.pixel(0, 0).map(|p| p.alpha()), Some(0));
    }
}
