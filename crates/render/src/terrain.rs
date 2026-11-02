//! A map's terrain (`ViewSettings::terrain`): the height of the ground under a point, from a DEM
//! raster layer or from elevation tiles (Terrarium or Mapbox Terrain-RGB), for a tilted view to
//! drape the map over (as MapLibre's terrain).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use rustgis_data::raster::Raster;
use rustgis_geom::{Coord, MERCATOR_R};
use rustgis_map::view::{TerrainEncoding, TerrainSource};
use rustgis_map::{Camera, Map};
use rustgis_tiles::cache::{self, Payload, State, Want};
use rustgis_tiles::template::{self, TileId};

/// Cells along each side of a tilted view's band laid over the relief, in the settled frame and
/// the moving drape alike: fine enough to follow it, few enough to draw quickly.
pub const CELLS: usize = 40;

/// The deepest zoom of elevation tiles read.
const MAX_TILE_ZOOM: u8 = 15;
/// How many zooms up a missing tile is stood in for by a coarser one already read.
const STAND_IN_LEVELS: u8 = 8;
/// How many zooms up the one coarser tile fetched along with a missing one is: cheap, and soon
/// there, so the relief shows while the sharper tile comes.
const COARSE_LEVELS: u8 = 4;

enum Heights {
    Raster {
        raster: Arc<Raster>,
        wrap_width: Option<i64>,
    },
    Tiles {
        url: String,
        encoding: TerrainEncoding,
        zoom: u8,
        /// The tiles read so far, and whether one is still on its way.
        read: RefCell<HashMap<TileId, Option<Arc<Payload>>>>,
        pending: RefCell<bool>,
        /// Tiles found on their way, not asked for again while this view is drawn.
        waiting: RefCell<std::collections::HashSet<TileId>>,
    },
}

/// The ground of a map's terrain, read for one view.
pub struct Ground {
    heights: Heights,
    exaggeration: f64,
    /// The map's body's ground over the Earth's (#223): heights are metres on it.
    body: f64,
}

impl Ground {
    /// The terrain of `map` as a view of `camera` sees it; `None` when the map has none (or
    /// isn't drawn in Web Mercator, which the heights are looked up in), or its DEM layer isn't
    /// there.
    pub fn of(map: &Map, camera: &Camera) -> Option<Ground> {
        let t = map.view.terrain.as_ref()?;
        if map.projection != rustgis_map::Projection::WebMercator {
            return None;
        }
        let exaggeration = if t.exaggeration.is_finite() { t.exaggeration.clamp(0.0, rustgis_map::view::MAX_EXAGGERATION) } else { 1.0 };
        let heights = match &t.source {
            TerrainSource::Layer(name) => {
                let raster = map.layer_by_name(name)?.raster.as_ref()?.data.clone()?;
                let wrap_width = crate::raster::full_turn_width(&raster);
                Heights::Raster { raster, wrap_width }
            }
            TerrainSource::Tiles { url, encoding } => {
                // Tiles a few screen pixels a cell at the view's centre: relief, not detail.
                let world = 2.0 * std::f64::consts::PI * MERCATOR_R;
                let z = (world / (256.0 * camera.res.max(1e-9) * 4.0)).log2();
                let zoom = if z.is_finite() { z.round().clamp(0.0, f64::from(MAX_TILE_ZOOM)) as u8 } else { 0 };
                Heights::Tiles {
                    url: url.clone(),
                    encoding: *encoding,
                    zoom,
                    read: RefCell::new(HashMap::new()),
                    pending: RefCell::new(false),
                    waiting: RefCell::new(std::collections::HashSet::new()),
                }
            }
        };
        Some(Ground { heights, exaggeration, body: map.body().ratio() })
    }

    /// The ground's height (metres, exaggerated) at a Web Mercator point; `None` where nothing
    /// is known (off the DEM, or no tile read yet).
    pub fn height(&self, m: Coord) -> Option<f64> {
        let h = match &self.heights {
            Heights::Raster { raster, wrap_width } => raster_height(raster, m, *wrap_width)?,
            Heights::Tiles { url, encoding, zoom, read, pending, waiting } => {
                tile_height(url, *encoding, *zoom, read, pending, waiting, m)?
            }
        };
        let h = h * self.exaggeration;
        h.is_finite().then_some(h)
    }

    /// How high the ground stands at `m` in a view of `cam`, in logical pixels at the centre's
    /// scale (what `Camera::project_raised` takes): its height over the metres a map unit spans
    /// there (Web Mercator stretches the ground toward the poles). 0 where nothing is known.
    pub fn up_px(&self, m: Coord, cam: &Camera) -> f64 {
        let Some(h) = self.height(m) else { return 0.0 };
        (h / self.per_unit(m) / cam.res.max(1e-9)).clamp(-1e7, 1e7)
    }

    /// The ground metres a map unit spans at `m` on the map's body (Web Mercator stretches the
    /// ground toward the poles): a height over it is in map units.
    pub fn per_unit(&self, m: Coord) -> f64 {
        per_unit(m, self.body)
    }

    /// Whether elevation tiles are still on their way (drawing again later shows more).
    pub fn pending(&self) -> bool {
        match &self.heights {
            Heights::Raster { .. } => false,
            Heights::Tiles { pending, .. } => *pending.borrow(),
        }
    }
}

/// A DEM's height at a Web Mercator point, read bilinearly from its first band; nodata and
/// points off it give `None`.
fn raster_height(r: &Raster, m: Coord, wrap_width: Option<i64>) -> Option<f64> {
    let n = r.transformer().from_merc(m)?;
    let (px, py) = r.transform.to_pixel(n);
    let px = r.wrap_col(px);
    let (fx, fy) = (px - 0.5, py - 0.5);
    if !(fx.is_finite() && fy.is_finite()) {
        return None;
    }
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let at = |dx: i64, dy: i64| -> Option<f64> {
        let x = x0 as i64 + dx;
        let x = wrap_width.map_or_else(|| x.clamp(0, i64::from(r.width) - 1), |w| if x < 0 || x >= w { x.rem_euclid(w) } else { x });
        let y = (y0 as i64 + dy).clamp(0, i64::from(r.height) - 1);
        r.value(0, x, y).map(f64::from).filter(|v| v.is_finite())
    };
    // Off the grid by more than half a cell: nothing.
    if px < 0.0 || py < 0.0 || px > f64::from(r.width) || py > f64::from(r.height) {
        return None;
    }
    let v00 = at(0, 0)?;
    let (v10, v01, v11) = (at(1, 0).unwrap_or(v00), at(0, 1).unwrap_or(v00), at(1, 1).unwrap_or(v00));
    let top = v00 + (v10 - v00) * tx;
    let bottom = v01 + (v11 - v01) * tx;
    Some(top + (bottom - top) * ty)
}

/// The tile at `zoom` holding a Web Mercator point, and where in it (0–1 across, down).
fn tile_at(m: Coord, zoom: u8) -> Option<(TileId, f64, f64)> {
    let half = std::f64::consts::PI * MERCATOR_R;
    let n = f64::from(1u32 << zoom.min(30));
    let (fx, fy) = ((m.x + half) / (2.0 * half) * n, (half - m.y) / (2.0 * half) * n);
    if !(fx.is_finite() && fy.is_finite()) || fx < 0.0 || fy < 0.0 || fx >= n || fy >= n {
        return None;
    }
    let id = TileId { z: zoom, x: fx.floor() as u32, y: fy.floor() as u32 };
    Some((id, fx.fract(), fy.fract()))
}

/// An elevation tile's height at a Web Mercator point: the tile at `zoom` once read (its fetch
/// starts here, with one [`COARSE_LEVELS`] up), meanwhile the nearest coarser one read already.
/// A failed fetch is tried again only after the cache's back-off.
fn tile_height(
    url: &str,
    encoding: TerrainEncoding,
    zoom: u8,
    read: &RefCell<HashMap<TileId, Option<Arc<Payload>>>>,
    pending: &RefCell<bool>,
    waiting: &RefCell<std::collections::HashSet<TileId>>,
    m: Coord,
) -> Option<f64> {
    let (id, _, _) = tile_at(m, zoom)?;
    let tile = |id: TileId, fetch: bool| -> Option<Arc<Payload>> {
        if let Some(t) = read.borrow().get(&id) {
            return t.clone();
        }
        if waiting.borrow().contains(&id) {
            return None;
        }
        let u = template::expand(url, id, 256, false, &[]);
        let got = if fetch {
            match cache::get_tile(&u, Want::Image) {
                State::Ready(p) => Some(p),
                State::Pending => {
                    *pending.borrow_mut() = true;
                    // Not asked again while this view is drawn: the next frame, drawn when it
                    // comes, asks again.
                    waiting.borrow_mut().insert(id);
                    return None;
                }
                State::Failed(_) => None,
            }
        } else {
            cache::peek(&u)
        };
        read.borrow_mut().insert(id, got.clone());
        got
    };
    let coarse = COARSE_LEVELS.min(zoom);
    let stand_ins = (0..=STAND_IN_LEVELS.min(zoom)).filter_map(|k| id.ancestor(k));
    for (k, t) in stand_ins.enumerate() {
        if let Some(p) = tile(t, k == 0 || k == usize::from(coarse))
            && let Payload::Image(pm) = &*p
            && let Some((_, fx, fy)) = tile_at(m, t.z)
        {
            return pixel_height(pm, encoding, fx, fy);
        }
    }
    None
}

/// The height an elevation tile's pixels give at (`fx`, `fy`) across it, bilinearly.
fn pixel_height(pm: &tiny_skia::Pixmap, encoding: TerrainEncoding, fx: f64, fy: f64) -> Option<f64> {
    let (w, h) = (pm.width() as i64, pm.height() as i64);
    if w == 0 || h == 0 {
        return None;
    }
    let (x, y) = (fx * w as f64 - 0.5, fy * h as f64 - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (tx, ty) = (x - x0, y - y0);
    let px = pm.pixels();
    let at = |dx: i64, dy: i64| -> Option<f64> {
        let (xi, yi) = ((x0 as i64 + dx).clamp(0, w - 1), (y0 as i64 + dy).clamp(0, h - 1));
        let p = px.get((yi * w + xi) as usize)?.demultiply();
        Some(encoding.height(p.red(), p.green(), p.blue()))
    };
    let (a, b, c, d) = (at(0, 0)?, at(1, 0)?, at(0, 1)?, at(1, 1)?);
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    Some(top + (bottom - top) * ty)
}

/// The ground metres a Web Mercator unit spans at `m` on a body `body` times the Earth's size.
fn per_unit(m: Coord, body: f64) -> f64 {
    (rustgis_map::projection::Flat::Mercator.ground_per_unit(m) * body).max(1e-9)
}

#[cfg(test)]
mod tests {
    use rustgis_data::raster::{ColorHint, Crs, GeoTransform};
    use rustgis_geom::lonlat_to_merc;

    use super::*;

    fn raster_ground(pixel_width: f64, crs: Crs, nodata: Option<f64>) -> Ground {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 90.0, pixel_width, pixel_height: -60.0 };
        let data = [0.0, 10.0, 20.0, 30.0].repeat(3);
        let raster = Arc::new(Raster::new("heights", 4, 3, vec![data], gt, crs, nodata, ColorHint::Gray, "Float32").unwrap());
        let wrap_width = crate::raster::full_turn_width(&raster);
        Ground { heights: Heights::Raster { raster, wrap_width }, exaggeration: 1.0, body: 1.0 }
    }

    #[test]
    fn raster_terrain_blends_across_both_ends_of_a_full_turn() {
        for crs in [Crs::LonLat, Crs::from_epsg(4269).unwrap()] {
            for pixel_width in [90.0, -90.0] {
                let ground = raster_ground(pixel_width, crs, None);
                for (col, expected) in [(0.0, 15.0), (0.25, 7.5), (3.75, 22.5)] {
                    let m = lonlat_to_merc(Coord::new(col * pixel_width, 0.0));
                    let h = ground.height(m).unwrap();
                    assert!((h - expected).abs() < 1e-8, "{pixel_width}, column {col}: {h} != {expected}");
                }
                let west = ground.height(lonlat_to_merc(Coord::new(-1e-6, 0.0))).unwrap();
                let east = ground.height(lonlat_to_merc(Coord::new(1e-6, 0.0))).unwrap();
                assert!((west - east).abs() < 1e-5, "{west} vs {east}");
            }
        }
    }

    #[test]
    fn raster_terrain_keeps_regional_projected_and_nodata_edges() {
        for (pixel_width, crs) in [(10.0, Crs::LonLat), (89.9, Crs::LonLat), (90.1, Crs::LonLat), (90.0, Crs::WebMercator)] {
            let ground = raster_ground(pixel_width, crs, None);
            let point = |col| {
                let c = Coord::new(col * pixel_width, 0.0);
                if crs == Crs::WebMercator { c } else { lonlat_to_merc(c) }
            };
            for (col, expected) in [(0.25, 0.0), (3.75, 30.0)] {
                let h = ground.height(point(col)).unwrap();
                assert!((h - expected).abs() < 1e-8, "{pixel_width}, column {col}: {h} != {expected}");
            }
            // A gap just west of the grid remains outside a regional or projected DEM.
            if pixel_width == 10.0 || crs == Crs::WebMercator {
                assert!(ground.height(point(-0.25)).is_none());
            }
        }
        let ground = raster_ground(90.0, Crs::LonLat, Some(30.0));
        assert!(ground.height(lonlat_to_merc(Coord::new(22.5, 0.0))).is_none(), "the wrapped base neighbour is nodata");
        assert!(ground.height(lonlat_to_merc(Coord::new(-22.5, 0.0))).is_none());
    }

    #[test]
    fn heights_stand_in_the_metres_of_the_body() {
        // A metre on Mars spans more of the (Earth-sized) map than one on the Earth (#223).
        let at = Coord::new(0.0, 0.0);
        let mars = rustgis_geom::Body::get("mars").ratio();
        assert!((per_unit(at, 1.0) / per_unit(at, mars) - 1.0 / mars).abs() < 1e-9);
    }

    #[test]
    fn elevation_tiles_decode_their_heights() {
        // Terrarium: 32768 m is zero.
        assert_eq!(TerrainEncoding::Terrarium.height(128, 0, 0), 0.0);
        assert_eq!(TerrainEncoding::Terrarium.height(131, 232, 0), 1000.0);
        // Terrain-RGB: −10000 m plus tenths of a metre.
        assert!((TerrainEncoding::Mapbox.height(1, 134, 160) - 0.0).abs() < 1e-9);
        // A tile's pixels, bilinearly.
        let mut pm = tiny_skia::Pixmap::new(2, 1).unwrap();
        pm.pixels_mut()[0] = tiny_skia::PremultipliedColorU8::from_rgba(128, 0, 0, 255).unwrap();
        pm.pixels_mut()[1] = tiny_skia::PremultipliedColorU8::from_rgba(128, 100, 0, 255).unwrap();
        let mid = pixel_height(&pm, TerrainEncoding::Terrarium, 0.5, 0.5).unwrap();
        assert!((mid - 50.0).abs() < 1e-9, "{mid}");
        // The tile holding a point.
        let (id, fx, fy) = tile_at(Coord::new(0.0, 0.0), 1).unwrap();
        assert_eq!((id.z, id.x, id.y), (1, 1, 1));
        assert!(fx.abs() < 1e-9 && fy.abs() < 1e-9);
        assert!(tile_at(Coord::new(0.0, 1e9), 3).is_none());
    }
}
