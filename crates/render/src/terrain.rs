//! A map's terrain (`ViewSettings::terrain`): the height of the ground under a point, from a DEM
//! raster layer or from elevation tiles (Terrarium or Mapbox Terrain-RGB), for a tilted view to
//! drape the map over (as MapLibre's terrain).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use rustgis_data::raster::Raster;
use rustgis_geom::{Coord, MERCATOR_R};
use rustgis_map::view::{TerrainEncoding, TerrainSource};
use rustgis_map::{Camera, Map, Rgba};
use rustgis_tiles::cache::{self, Payload, State, Want};
use rustgis_tiles::template::{self, TileId};

mod water;

pub(crate) use water::{Glint, SHORE_PX, Water, paint_shore};

/// Cells along each side of a tilted view's band laid over the relief while the map moves (the
/// drape, `tilt_drape` in the app): few enough to lay out every frame.
pub const CELLS: usize = 40;

/// The fewest cells along the longer side of a band laid over the relief in the frame that stays
/// on screen once the map settles, and in exports (#1523; [`settled_cells`] grows it with the
/// picture, #1882; [`settled_grid`] the shorter side, #2037): fine enough that a ridge's outline
/// follows the ridge, where 40 drew it in long straight lines and let a massif sink into its
/// surroundings.
pub const SETTLED_CELLS: usize = 4 * CELLS;

/// The most cells along the longer side of a band laid over the relief in the settled frame and
/// in exports (#1882): 1280 showed no more of a ridge than 640, for about three times the memory.
pub const MAX_SETTLED_CELLS: usize = 4 * SETTLED_CELLS;

/// Pixels of a band's picture to each of its mesh's cells in the settled frame and in exports
/// (#1882): a finer mesh outruns the picture draped over it.
const PX_PER_SETTLED_CELL: u32 = 4;

/// Cells along the longer side of a band laid over the relief in the settled frame and in exports
/// (#1882): one every [`PX_PER_SETTLED_CELL`] pixels of the longer side of the band's picture,
/// `picture_px`, and no more than `dem_cells`, the ground's own cells along that side of the band
/// ([`Ground::cells_along`], `None` when unknown); never fewer than [`SETTLED_CELLS`] (no
/// coarser than before) nor more than [`MAX_SETTLED_CELLS`].
pub fn settled_cells(picture_px: u32, dem_cells: Option<f64>) -> usize {
    let mut n = (picture_px / PX_PER_SETTLED_CELL) as usize;
    if let Some(d) = dem_cells.filter(|d| d.is_finite()) {
        n = n.min(d.max(0.0).ceil() as usize);
    }
    n.clamp(SETTLED_CELLS, MAX_SETTLED_CELLS)
}

/// Cells across and down a band's mesh in the settled frame and in exports (#2037), its picture
/// `picture` (width, height) pixels and the ground's own cells across and down the band
/// `dem_cells` ([`Ground::cells_along`], `None` when unknown): along the picture's longer side,
/// [`settled_cells`] of it; along the shorter, as many as keep the cells the same size in the
/// picture, no more than the ground's cells along that side nor fewer than [`CELLS`]. A band
/// wider than tall no longer pays its long side's cells down its short one, finer there than
/// the picture.
pub fn settled_grid(picture: (u32, u32), dem_cells: (Option<f64>, Option<f64>)) -> (usize, usize) {
    let wide = picture.0 >= picture.1;
    let ((long, short), (dem_long, dem_short)) =
        if wide { (picture, dem_cells) } else { ((picture.1, picture.0), (dem_cells.1, dem_cells.0)) };
    let n = settled_cells(long, dem_long);
    // In u64: the product overflows a 32-bit usize (wasm) once a side passes 6.7 M px.
    let mut m = (n as u64 * u64::from(short)).div_ceil(u64::from(long.max(1))) as usize;
    if let Some(d) = dem_short.filter(|d| d.is_finite()) {
        m = m.min(d.max(0.0).ceil() as usize);
    }
    let m = m.max(CELLS);
    if wide { (n, m) } else { (m, n) }
}

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

/// A map's block diagram as a frame draws it (#1914, `TerrainBlock`): the box the ground is cut
/// to (Web Mercator) and the height of its base.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    /// The base's height: metres, exaggerated as [`Ground::height`]'s are.
    pub base: f64,
    /// Whether the base is worked out from every height there will be: false while elevation
    /// tiles are still on their way ([`Ground::pending`]), when the base would move with each one
    /// that arrives, so the walls wait (the ground is cut all the same).
    pub settled: bool,
    pub walls: Rgba,
    /// A round block (#2318): its circle's centre and radius (Web Mercator), the box above being
    /// the one round it. None: the box.
    pub circle: Option<(Coord, f64)>,
}

impl Block {
    /// Whether the walls stand now: the base is settled and no elevation tile is still on its way.
    /// A tile asked for after the block was read (the mesh's finer samples, say) can make the
    /// base move, so `settled` alone is not enough once the ground has been sampled.
    pub fn walls_stand(&self, ground: &Ground) -> bool {
        self.settled && !ground.pending()
    }
}

/// Points along each side of the grid of heights read for a block's lowest and highest ground.
const BLOCK_SAMPLES: usize = 64;

/// The ground of a map's terrain, read for one view.
pub struct Ground {
    heights: Heights,
    exaggeration: f64,
    /// The map's body's ground over the Earth's (#223): heights are metres on it.
    body: f64,
    /// Water flat at its level over the heights (#1915), when the terrain names a water layer.
    water: Option<Arc<water::Water>>,
}

impl Ground {
    /// The terrain of `map` as a view of `camera` sees it; `None` when the map has none (or
    /// isn't drawn in Web Mercator, which the heights are looked up in), or its DEM layer isn't
    /// there.
    pub fn of(map: &Map, camera: &Camera) -> Option<Ground> {
        let mut ground = Ground::heights_of(map, camera)?;
        ground.attach_water(map, camera);
        Some(ground)
    }

    /// Sets the water of `map`'s terrain over these heights. A water read while shore tiles were
    /// on their way, taken again from its cache, leaves these heights pending too: inside its
    /// lakes no tile is read, and a frame or an export waiting for tiles must still wait for the
    /// levels to be read again.
    fn attach_water(&mut self, map: &Map, camera: &Camera) {
        self.water = water::of(map, camera, self);
        if self.water.as_ref().is_some_and(|w| w.waits_for_tiles())
            && let Heights::Tiles { pending, .. } = &self.heights
        {
            *pending.borrow_mut() = true;
        }
    }

    /// How many polygons of `map`'s water layer have their shore read at wider steps than half a
    /// cell of the heights a view of `camera` reads (#1915; see [`Ground::water_coarse`]), from
    /// their shapes alone: nothing is read, no elevation tile asked for. 0 without water.
    pub fn water_coarse_unread(map: &Map, camera: &Camera) -> usize {
        Ground::heights_of(map, camera).map_or(0, |g| water::coarse_shores(map, camera, &g))
    }

    /// [`Ground::of`] without its water.
    fn heights_of(map: &Map, camera: &Camera) -> Option<Ground> {
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
        Some(Ground { heights, exaggeration, body: map.body().ratio(), water: None })
    }

    /// The ground's height (metres, exaggerated) at a Web Mercator point: a water body's level
    /// inside one (#1915); `None` where nothing is known (off the DEM, or no tile read yet).
    pub fn height(&self, m: Coord) -> Option<f64> {
        let h = self.water.as_ref().and_then(|w| w.level(m)).or_else(|| self.raw_height(m))?;
        let h = h * self.exaggeration;
        h.is_finite().then_some(h)
    }

    /// The heights' own height (metres, before exaggeration, water left out) at a Web Mercator
    /// point.
    fn raw_height(&self, m: Coord) -> Option<f64> {
        match &self.heights {
            Heights::Raster { raster, wrap_width } => raster_height(raster, m, *wrap_width),
            Heights::Tiles { url, encoding, zoom, read, pending, waiting } => {
                tile_height(url, *encoding, *zoom, read, pending, waiting, m).0
            }
        }
    }

    /// A usable shore height and whether its requested detail was read. A ready ancestor can
    /// supply the height after a detailed tile failed, but the shore must still be read again.
    fn shore_height(&self, m: Coord) -> (Option<f64>, bool) {
        match &self.heights {
            Heights::Raster { raster, wrap_width } => (raster_height(raster, m, *wrap_width), true),
            Heights::Tiles { url, encoding, zoom, read, pending, waiting } => tile_height(url, *encoding, *zoom, read, pending, waiting, m),
        }
    }

    /// The terrain's water (#1915), when it names a water layer: what Shaded Relief draws with
    /// the even light of flat ground.
    pub(crate) fn water(&self) -> Option<&Arc<Water>> {
        self.water.as_ref()
    }

    /// How many polygons of the terrain's water layer have no level, so stay on the ground: no
    /// height known along their shore and no level given (#1915). 0 without water.
    pub fn water_unlevelled(&self) -> usize {
        self.water.as_ref().map_or(0, |w| w.unlevelled)
    }

    /// How many polygons of the terrain's water layer had their shore read at wider steps than
    /// half the ground's cell, to keep a long shore's reading bounded: their level may lie a
    /// little above their lowest shore (#1915). 0 without water.
    pub fn water_coarse(&self) -> usize {
        self.water.as_ref().map_or(0, |w| w.coarse)
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

    /// How much the heights are stretched (`Terrain::exaggeration`, as read).
    pub(crate) fn exaggeration(&self) -> f64 {
        self.exaggeration
    }

    /// Where the Web Mercator point `m` falls on the ground's grid, in its cells (column, row):
    /// a DEM's pixels; elevation tiles' pixels at the zoom read. `None` off the DEM's plane.
    pub fn grid_point(&self, m: Coord) -> Option<(f64, f64)> {
        match &self.heights {
            Heights::Raster { raster, .. } => raster.transformer().from_merc(m).map(|n| raster.transform.to_pixel(n)),
            Heights::Tiles { zoom, .. } => {
                let half = std::f64::consts::PI * MERCATOR_R;
                let n = 256.0 * f64::from(1u32 << (*zoom).min(30));
                Some(((m.x + half) / (2.0 * half) * n, (half - m.y) / (2.0 * half) * n))
            }
        }
    }

    /// How many of the ground's own cells span the stretch of ground with these `corners` (Web
    /// Mercator), across and down their box in the ground's grid (#2037): a DEM's cells, the
    /// box clipped to the DEM (a geographic DEM's columns met again a whole turn round, as
    /// [`raster_height`] reads them; a whole turn's not clipped at all); elevation tiles'
    /// pixels at the zoom read. `None` when no corner lands on the grid's plane.
    pub fn cells_across(&self, corners: &[Coord]) -> Option<(f64, f64)> {
        let pixels = corners.iter().filter_map(|m| self.grid_point(*m));
        // `turn`: a geographic DEM's columns in 360°, where `raster_height` meets it again.
        let ((w, h), full_turn, turn) = match &self.heights {
            Heights::Raster { raster, wrap_width } => {
                let t = raster.transformer();
                let turn = t.is_geographic().then(|| 360.0 / raster.transform.pixel_width.abs()).filter(|t| t.is_finite());
                ((f64::from(raster.width), f64::from(raster.height)), wrap_width.is_some(), turn)
            }
            Heights::Tiles { zoom, .. } => {
                let n = 256.0 * f64::from(1u32 << (*zoom).min(30));
                ((n, n), false, None)
            }
        };
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (x, y) in pixels.filter(|(x, y)| x.is_finite() && y.is_finite()) {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        if x0 > x1 {
            return None;
        }
        let across = |a: f64, b: f64, size: f64| (b.min(size) - a.max(0.0)).max(0.0);
        let x = match turn {
            // A whole turn's columns go on past either edge: no edge to clip at, one turn at most.
            _ if full_turn => (x1 - x0).clamp(0.0, w),
            // Moved round by the whole turns that put most of the stretch on the DEM (a 0–360°
            // grid, a stretch across the seam from a DEM beside it, a view a turn away).
            Some(t) => {
                let k = ((w - x0 - x1) / 2.0 / t).round();
                [k - 1.0, k, k + 1.0].iter().map(|k| across(x0 + k * t, x1 + k * t, w)).fold(0.0, f64::max)
            }
            None => across(x0, x1, w),
        };
        Some((x, across(y0, y1, h)))
    }

    /// How many of the ground's own cells lie along each side of a band with these `corners`
    /// (Web Mercator; top left, top right, bottom left, bottom right), across and down (#2037):
    /// the box of [`Ground::cells_across`] measured along each side as it lies on the grid (its
    /// top and bottom edges across, its left and right edges down, each on its own: a grid of
    /// cells longer than wide doesn't keep a band's sides square), so a band turned a quarter on
    /// it takes its rows across and its columns down. That is the grid's spacing along the side,
    /// the finest detail it holds there, not the cells a diagonal side passes through. A side
    /// whose direction is unknown (a corner off the grid's plane, edges that cancel out) takes
    /// the box's longer side.
    pub fn cells_along(&self, corners: &[Coord; 4]) -> Option<(f64, f64)> {
        let (cols, rows) = self.cells_across(corners)?;
        let longer = cols.max(rows);
        let [a, b, c, d] = corners.map(|m| self.grid_point(m).filter(|(x, y)| x.is_finite() && y.is_finite()));
        // Two edges' sum, the side's direction on the grid; the box's extent along it.
        let along = |p: Option<(f64, f64)>, q: Option<(f64, f64)>, r: Option<(f64, f64)>, t: Option<(f64, f64)>| {
            let ((p, q), (r, t)) = (p.zip(q)?, r.zip(t)?);
            let (x, y) = (q.0 - p.0 + t.0 - r.0, q.1 - p.1 + t.1 - r.1);
            let len = x.hypot(y);
            (len > 0.0 && len.is_finite()).then(|| cols * (x / len).abs() + rows * (y / len).abs())
        };
        Some((along(a, b, c, d).unwrap_or(longer), along(a, c, b, d).unwrap_or(longer)))
    }

    /// The block diagram `map`'s terrain is cut to, when it has one: its box (the DEM's extent
    /// when none is given; none over elevation tiles then) or the circle in it, and its base
    /// `depth` metres below the lowest ground read in it (on a grid of [`BLOCK_SAMPLES`] cells a
    /// side, inside the circle for a round block; where a DEM has no
    /// height, 0, as the ground is drawn there; while elevation tiles are on their way, not
    /// [`Block::settled`]), or
    /// half its relief, at least `BLOCK_MIN_DEPTH`, both exaggerated as the heights are.
    ///
    /// Elevation tiles are read for it at a zoom the box spans at most two tiles of, so the grid
    /// asks for a few tiles however large the box: its lowest and highest ground only set the
    /// base, which a coarser tile serves.
    pub fn block(&self, map: &Map) -> Option<Block> {
        let b = map.view.terrain.as_ref()?.block.as_ref()?;
        let [x0, y0, x1, y1] = match (b.extent, &self.heights) {
            (Some(e), _) => e,
            (None, Heights::Raster { raster, .. }) => raster_box(raster)?,
            (None, Heights::Tiles { .. }) => return None,
        };
        // A round block (#2318): the largest circle in the box, or a smaller one, centred in it;
        // the box is then the one round the circle.
        let circle = match b.shape {
            rustgis_map::view::BlockShape::Box => None,
            rustgis_map::view::BlockShape::Circle { radius } => {
                let r = (x1 - x0).min(y1 - y0) / 2.0;
                Some((Coord::new((x0 + x1) / 2.0, (y0 + y1) / 2.0), radius.map_or(r, |q| q.min(r))))
            }
        };
        let [x0, y0, x1, y1] = circle.map_or([x0, y0, x1, y1], |(c, r)| [c.x - r, c.y - r, c.x + r, c.y + r]);
        let world = 2.0 * std::f64::consts::PI * MERCATOR_R;
        let coarse = (world / (x1 - x0).max(y1 - y0).max(1e-9)).log2().floor();
        let coarse = if coarse.is_finite() { coarse.clamp(0.0, f64::from(MAX_TILE_ZOOM)) as u8 } else { 0 };
        let sample = |m: Coord| match &self.heights {
            Heights::Raster { .. } => self.height(m),
            // A water body's level first, as `height` takes it, so the ground drawn in a lake
            // counts at its level.
            Heights::Tiles { url, encoding, zoom, read, pending, waiting } => {
                let raw = self
                    .water
                    .as_ref()
                    .and_then(|w| w.level(m))
                    .or_else(|| tile_height(url, *encoding, coarse.min(*zoom), read, pending, waiting, m).0)?;
                let h = raw * self.exaggeration;
                h.is_finite().then_some(h)
            }
        };
        let n = BLOCK_SAMPLES as f64;
        let (mut lo, mut hi, mut missing) = (f64::INFINITY, f64::NEG_INFINITY, false);
        for (i, j) in (0..=BLOCK_SAMPLES).flat_map(|j| (0..=BLOCK_SAMPLES).map(move |i| (i as f64, j as f64))) {
            let m = Coord::new(x0 + (x1 - x0) * i / n, y0 + (y1 - y0) * j / n);
            // A round block's ground off its circle isn't drawn, so doesn't set its base.
            if circle.is_some_and(|(c, r)| (m.x - c.x).hypot(m.y - c.y) > r * (1.0 + 1e-9)) {
                continue;
            }
            match sample(m) {
                Some(h) => (lo, hi) = (lo.min(h), hi.max(h)),
                None => missing = true,
            }
        }
        // Elevation tiles still on their way: the base would move with each one that arrives.
        let settled = !self.pending();
        // Ground with no height (a DEM's nodata or ground off it, a tile that failed) is drawn at
        // 0 (`up_px`): once settled, the base goes below it too.
        if missing && settled {
            lo = lo.min(0.0);
        }
        if lo > hi {
            hi = lo.min(0.0);
            lo = hi;
        }
        let depth = match b.depth {
            Some(d) => d * self.exaggeration,
            None => (0.5 * (hi - lo)).max(rustgis_map::view::BLOCK_MIN_DEPTH * self.exaggeration),
        };
        Some(Block { x0, y0, x1, y1, base: lo - depth, settled, walls: b.walls, circle })
    }

    /// Whether elevation tiles are still on their way (drawing again later shows more).
    pub fn pending(&self) -> bool {
        match &self.heights {
            Heights::Raster { .. } => false,
            Heights::Tiles { pending, .. } => *pending.borrow(),
        }
    }
}

/// The layer called `name` on `map` that a block diagram's walls are coloured by (#1914), when it
/// can be: the only layer of that name, of polygons, holding them all (not read a view at a
/// time), as `map.terrain` checks it. Otherwise the walls are their own colour.
pub fn walls_layer<'a>(map: &'a Map, name: &str) -> Option<&'a rustgis_map::Layer> {
    let mut named = map.layers.iter().filter(|l| l.name.eq_ignore_ascii_case(name));
    let l = named.next()?;
    let polygons = l.geometry_type() == rustgis_data::GeometryType::Polygon;
    (named.next().is_none() && polygons && !l.is_streamed()).then_some(l)
}

/// The largest north-up Web Mercator box inside a DEM's corners, near enough (the second
/// smallest and second largest of their x and of their y): its whole extent for a DEM in
/// geographic or Web Mercator coordinates, a little less for one turned a little by its
/// projection (UTM). `None` when a corner can't be placed.
pub fn raster_box(r: &Raster) -> Option<[f64; 4]> {
    let (xs, ys) = raster_corners(r)?;
    let b = [xs[1], ys[1], xs[2], ys[2]];
    (b.iter().all(|v| v.is_finite()) && b[0] < b[2] && b[1] < b[3]).then_some(b)
}

/// The outer bounds of a DEM's footprint, its four corners' extremes (Web Mercator, `[xmin, ymin,
/// xmax, ymax]`): every height the DEM gives lies inside them, so a box outside them misses it
/// altogether, where a box can still reach its corners and miss [`raster_box`]. `None` when a
/// corner can't be placed.
pub fn raster_footprint(r: &Raster) -> Option<[f64; 4]> {
    let (xs, ys) = raster_corners(r)?;
    let b = [xs[0], ys[0], xs[3], ys[3]];
    (b.iter().all(|v| v.is_finite()) && b[0] < b[2] && b[1] < b[3]).then_some(b)
}

/// The x and the y of a DEM's four corners in Web Mercator, each sorted; `None` when a corner
/// can't be placed.
fn raster_corners(r: &Raster) -> Option<([f64; 4], [f64; 4])> {
    let gt = &r.transform;
    let (w, h) = (gt.pixel_width * f64::from(r.width), gt.pixel_height * f64::from(r.height));
    let t = r.transformer();
    let mut xs = [0.0; 4];
    let mut ys = [0.0; 4];
    for (k, (dx, dy)) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].into_iter().enumerate() {
        let m = rustgis_geom::lonlat_to_merc(t.to_lonlat(Coord::new(gt.origin_x + dx, gt.origin_y + dy))?);
        (xs[k], ys[k]) = (m.x, m.y);
    }
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);
    Some((xs, ys))
}

/// A DEM's first-band height at a Web Mercator point, before exaggeration or water. Uses
/// the same bounded bilinear sampling and full-turn wrapping as [`Ground`]; nodata, empty
/// grids and points off the DEM give `None`.
pub fn dem_height(raster: &Raster, at: Coord) -> Option<f64> {
    raster_height(raster, at, crate::raster::full_turn_width(raster))
}

fn raster_height(r: &Raster, m: Coord, wrap_width: Option<i64>) -> Option<f64> {
    if r.width == 0 || r.height == 0 {
        return None;
    }
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
/// A failed fetch is tried again only after the cache's back-off. The flag says the height
/// came from the requested detail tile, so shore sampling needs no second lookup.
fn tile_height(
    url: &str,
    encoding: TerrainEncoding,
    zoom: u8,
    read: &RefCell<HashMap<TileId, Option<Arc<Payload>>>>,
    pending: &RefCell<bool>,
    waiting: &RefCell<std::collections::HashSet<TileId>>,
    m: Coord,
) -> (Option<f64>, bool) {
    let Some((id, _, _)) = tile_at(m, zoom) else { return (None, false) };
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
            return (pixel_height(pm, encoding, fx, fy), k == 0);
        }
    }
    (None, false)
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
        Ground { heights: Heights::Raster { raster, wrap_width }, exaggeration: 1.0, body: 1.0, water: None }
    }

    #[test]
    fn dem_sampler_refuses_empty_grid_dimensions() {
        let mut raster = Raster::new(
            "DEM",
            2,
            1,
            vec![vec![10.0, 20.0]],
            GeoTransform { origin_x: 0.0, origin_y: 100.0, pixel_width: 100.0, pixel_height: -100.0 },
            Crs::WebMercator,
            None,
            ColorHint::Gray,
            "Float32",
        )
        .unwrap();
        let at = Coord::new(0.0, 50.0);
        assert!(dem_height(&raster, at).is_some());
        raster.width = 0;
        assert_eq!(dem_height(&raster, at), None);
        raster.width = 2;
        raster.height = 0;
        assert_eq!(dem_height(&raster, Coord::new(50.0, 100.0)), None);
    }

    #[test]
    fn a_blocks_base_goes_below_the_nodata_drawn_at_zero() {
        // #1914: ground 5000 m high beside nodata (drawn at 0): the base is 1000 m below 0, not
        // below 5000 m, so the walls still go down.
        let gt = GeoTransform { origin_x: 0.0, origin_y: 100.0, pixel_width: 100.0, pixel_height: -100.0 };
        let raster =
            Raster::new("dem", 2, 1, vec![vec![5000.0, -9999.0]], gt, Crs::WebMercator, Some(-9999.0), ColorHint::Gray, "Float32").unwrap();
        let mut m = Map::new("Map");
        m.add_raster("dem", rustgis_map::RasterLayer::from_path(raster, "dem.tif"));
        let block = rustgis_map::view::TerrainBlock {
            extent: Some([0.0, 0.0, 200.0, 100.0]),
            depth: Some(1000.0),
            walls: rustgis_map::view::BLOCK_WALLS,
            walls_layer: None,
            water: None,
            shape: rustgis_map::view::BlockShape::Box,
        };
        m.view.terrain = Some(rustgis_map::view::Terrain {
            source: rustgis_map::view::TerrainSource::Layer("dem".into()),
            exaggeration: 1.0,
            block: Some(block),
            water: None,
        });
        let camera = Camera { center: Coord::new(100.0, 50.0), res: 1.0, bearing: 0.0, pitch: 60.0 };
        let b = Ground::of(&m, &camera).and_then(|g| g.block(&m)).expect("a block");
        assert!((b.base + 1000.0).abs() < 1e-6, "{}", b.base);
    }

    #[test]
    fn a_blocks_walls_wait_for_elevation_tiles_on_their_way() {
        // #1914: over elevation tiles, the base isn't settled while tiles are on their way (it
        // would move with each); once none is, a tile that failed counts as 0, as it's drawn.
        let mut read = HashMap::new();
        for z in 0..=2u8 {
            for x in 0..(1u32 << z) {
                for y in 0..(1u32 << z) {
                    read.insert(TileId { z, x, y }, None);
                }
            }
        }
        let ground = |pending: bool| Ground {
            heights: Heights::Tiles {
                url: "https://example.invalid/{z}/{x}/{y}.png".into(),
                encoding: TerrainEncoding::Terrarium,
                zoom: 2,
                read: RefCell::new(read.clone()),
                pending: RefCell::new(pending),
                waiting: RefCell::new(std::collections::HashSet::new()),
            },
            exaggeration: 1.0,
            body: 1.0,
            water: None,
        };
        let mut m = Map::new("Map");
        let block = rustgis_map::view::TerrainBlock {
            extent: Some([0.0, 0.0, 5000.0, 5000.0]),
            depth: Some(300.0),
            walls: rustgis_map::view::BLOCK_WALLS,
            walls_layer: None,
            water: None,
            shape: rustgis_map::view::BlockShape::Box,
        };
        m.view.terrain = Some(rustgis_map::view::Terrain {
            source: rustgis_map::view::TerrainSource::Tiles {
                url: "https://example.invalid/{z}/{x}/{y}.png".into(),
                encoding: TerrainEncoding::Terrarium,
            },
            exaggeration: 1.0,
            block: Some(block),
            water: None,
        });
        let waiting = ground(true).block(&m).expect("a block");
        assert!(!waiting.settled);
        let done = ground(false).block(&m).expect("a block");
        assert!(done.settled && (done.base + 300.0).abs() < 1e-6, "{done:?}");
        // The block read from cached tiles, then finer mesh samples asking for tiles on their way:
        // the walls wait again, though the base was settled when it was read.
        let flat = ground(false);
        let cached = flat.block(&m).expect("a block");
        assert!(cached.walls_stand(&flat));
        if let Heights::Tiles { pending, .. } = &flat.heights {
            *pending.borrow_mut() = true;
        }
        assert!(cached.settled && !cached.walls_stand(&flat));
    }

    #[test]
    fn a_turned_dems_footprint_reaches_past_its_box() {
        // A UTM DEM 200 km west of its central meridian is turned by its projection in Web
        // Mercator: its footprint (the outer bounds of its corners) reaches past the largest box
        // inside them, where a box given by the user may still reach its corners.
        let gt = GeoTransform { origin_x: 300_000.0, origin_y: 5_000_000.0, pixel_width: 1000.0, pixel_height: -1000.0 };
        let utm = Crs::from_epsg(32633).unwrap();
        let raster = Raster::new("dem", 100, 100, vec![vec![0.0; 10_000]], gt, utm, None, ColorHint::Gray, "Float32").unwrap();
        let (b, f) = (raster_box(&raster).expect("a box"), raster_footprint(&raster).expect("a footprint"));
        assert!(f[0] <= b[0] && f[1] <= b[1] && b[2] <= f[2] && b[3] <= f[3], "{f:?} {b:?}");
        assert!(f != b, "{f:?}");
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
    fn the_settled_mesh_follows_the_picture_within_its_bounds() {
        // #1882: one cell every 4 px, within 160 and 640.
        for (px, n) in [(0, 160), (400, 160), (640, 160), (1600, 400), (2000, 500), (2560, 640), (4800, 640), (u32::MAX, 640)] {
            assert_eq!(settled_cells(px, None), n, "{px} px");
        }
        // Never past the DEM's cells across the band, nor coarser than 160.
        assert_eq!(settled_cells(4800, Some(300.0)), 300);
        assert_eq!(settled_cells(4800, Some(299.2)), 300);
        assert_eq!(settled_cells(1600, Some(1924.0)), 400);
        assert_eq!(settled_cells(4800, Some(100.0)), SETTLED_CELLS);
        assert_eq!(settled_cells(4800, Some(0.0)), SETTLED_CELLS);
        assert_eq!(settled_cells(1600, Some(f64::NAN)), 400);
        assert_eq!(settled_cells(4800, Some(f64::INFINITY)), 640);
    }

    #[test]
    fn the_settled_mesh_keeps_its_cells_square_in_the_picture() {
        // #2037: the bands of a 1600 × 1100 export of #2017's view, at 96 and 288 dpi. The longer
        // side as before; the shorter with cells of the same size.
        for (picture, cells) in [
            ((2359, 1343), (589, 336)),
            ((1668, 950), (417, 238)),
            ((1179, 451), (294, 113)),
            ((4096, 2332), (640, 365)),
            ((3537, 1353), (640, 245)),
            ((1343, 2359), (336, 589)),
            ((1600, 1600), (400, 400)),
        ] {
            assert_eq!(settled_grid(picture, (None, None)), cells, "{picture:?}");
        }
        // The shorter side no coarser than the moving drape's, the longer no coarser than 160.
        assert_eq!(settled_grid((4000, 100), (None, None)), (640, CELLS));
        assert_eq!(settled_grid((0, 0), (None, None)), (SETTLED_CELLS, CELLS));
        assert_eq!(settled_grid((400, 300), (None, None)), (SETTLED_CELLS, SETTLED_CELLS * 3 / 4));
        // Each side no finer than the ground's cells along it (the longer side floored at 160).
        assert_eq!(settled_grid((2359, 1343), (Some(1000.0), Some(200.0))), (589, 200));
        assert_eq!(settled_grid((2359, 1343), (Some(300.0), Some(1000.0))), (300, 171));
        assert_eq!(settled_grid((2359, 1343), (Some(100.0), Some(10.0))), (SETTLED_CELLS, CELLS));
        assert_eq!(settled_grid((2359, 1343), (Some(f64::NAN), Some(f64::INFINITY))), (589, 336));
        // No overflow on a 32-bit usize.
        assert_eq!(settled_grid((u32::MAX, u32::MAX), (None, None)), (MAX_SETTLED_CELLS, MAX_SETTLED_CELLS));
    }

    #[test]
    fn a_band_counts_the_ground_cells_along_its_own_sides() {
        // #2037: a Web Mercator DEM of 1 km cells, 400 × 400; a band 40 km across and 10 km down.
        let gt = GeoTransform { origin_x: -200_000.0, origin_y: 200_000.0, pixel_width: 1000.0, pixel_height: -1000.0 };
        let raster = Arc::new(
            Raster::new("heights", 400, 400, vec![vec![0.0; 160_000]], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap(),
        );
        let ground = Ground { heights: Heights::Raster { raster, wrap_width: None }, exaggeration: 1.0, body: 1.0, water: None };
        let band = |deg: f64| {
            let (s, c) = deg.to_radians().sin_cos();
            [(-20_000.0, 5_000.0), (20_000.0, 5_000.0), (-20_000.0, -5_000.0), (20_000.0, -5_000.0)]
                .map(|(x, y)| Coord::new(x * c - y * s, x * s + y * c))
        };
        let close = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6;
        // North up: 40 across, 10 down; turned a quarter, still 40 along it, its box's rows.
        let north = ground.cells_along(&band(0.0)).unwrap();
        assert!(close(north, (40.0, 10.0)), "{north:?}");
        let quarter = ground.cells_along(&band(90.0)).unwrap();
        assert!(close(quarter, (40.0, 10.0)), "{quarter:?}");
        assert!(close(ground.cells_across(&band(90.0)).unwrap(), (10.0, 40.0)));
        // Turned 30°: its box measured along each side, no less than the side itself.
        let turned = ground.cells_along(&band(30.0)).unwrap();
        assert!(turned.0 >= 40.0 && turned.1 >= 10.0 && turned.0 > turned.1, "{turned:?}");
        // Off the DEM: no cells; partly on: the part on it.
        assert_eq!(ground.cells_along(&band(0.0).map(|m| Coord::new(m.x + 1e6, m.y + 1e6))), Some((0.0, 0.0)));
        let half_on = ground.cells_along(&band(0.0).map(|m| Coord::new(m.x + 200_000.0, m.y))).unwrap();
        assert!(close(half_on, (20.0, 10.0)), "{half_on:?}");
        // Top and bottom edges running opposite ways: no direction across, the box's longer side.
        let crossed = [band(0.0)[0], band(0.0)[1], band(0.0)[3], band(0.0)[2]];
        assert!(close(ground.cells_along(&crossed).unwrap(), (40.0, 10.0)), "{:?}", ground.cells_along(&crossed));
        // Cells ten times taller than wide (1 km × 10 km): a band 10 km down by 40 km across, its
        // long side turned to point 45° up on the map, lies 28.3 columns and 2.83 rows along it,
        // and 7.07 columns and 0.71 rows down. Each side measured along its own direction.
        let gt = GeoTransform { origin_x: -200_000.0, origin_y: 2_000_000.0, pixel_width: 1000.0, pixel_height: -10_000.0 };
        let raster = Arc::new(
            Raster::new("heights", 400, 400, vec![vec![0.0; 160_000]], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap(),
        );
        let tall = Ground { heights: Heights::Raster { raster, wrap_width: None }, exaggeration: 1.0, body: 1.0, water: None };
        let (cols, rows) = tall.cells_across(&band(45.0)).unwrap();
        let (x, y) = tall.cells_along(&band(45.0)).unwrap();
        // Across: direction (28.3, -2.83) on the grid; down: (7.07, 0.71), each normalised.
        let unit = |x: f64, y: f64| (x / x.hypot(y), y / x.hypot(y));
        let (ax, ay) = unit(1.0, 0.1);
        let (dx, dy) = unit(1.0, -0.1);
        assert!((x - (cols * ax + rows * ay)).abs() < 1e-6, "{x} from {cols} × {rows}");
        assert!((y - (cols * dx + rows * dy.abs())).abs() < 1e-6, "{y} from {cols} × {rows}");
        // Both sides about as many as the box's columns: neither is cut to its rows.
        assert!(x > 0.9 * cols && y > 0.9 * cols, "{x}, {y} from {cols} × {rows}");
    }

    #[test]
    fn a_stretch_spans_the_ground_cells_under_it() {
        // A 4 × 3 grid of 90° by 60° cells, the whole Earth: a stretch over two columns spans 2
        // cells, one past its edges only the grid's.
        let ground = raster_ground(90.0, Crs::LonLat, None);
        let at = |lon: f64, lat: f64| lonlat_to_merc(Coord::new(lon, lat));
        let two = ground.cells_across(&[at(0.0, 30.0), at(180.0, 30.0), at(0.0, -30.0), at(180.0, -30.0)]).unwrap();
        assert!((two.0 - 2.0).abs() < 1e-6 && (two.1 - 1.0).abs() < 1e-6, "{two:?}");
        let all = ground.cells_across(&[Coord::new(-1e8, 1e8), Coord::new(1e8, -1e8)]).unwrap();
        assert!(all.0 <= 4.0 + 1e-9 && all.1 <= 3.0 + 1e-9, "{all:?}");
        assert_eq!(ground.cells_across(&[]), None);
        // Across the seam, the whole stretch: 135° to 225° (-135°) spans one cell, not the half
        // cell short of the grid's edge; the same stretch a turn away likewise.
        let seam = ground.cells_across(&[at(135.0, 30.0), at(225.0, 30.0)]).unwrap().0;
        assert!((seam - 1.0).abs() < 1e-6, "{seam}");
        let away = ground.cells_across(&[at(495.0, 30.0), at(585.0, 30.0)]).unwrap().0;
        assert!((away - 1.0).abs() < 1e-6, "{away}");
        // A regional DEM of 1° cells beside the seam: a stretch across the seam from it counts
        // the cells it reaches there, as its heights are read, though its middle is off the DEM.
        let regional = |west: f64| {
            let gt = GeoTransform { origin_x: west, origin_y: 90.0, pixel_width: 1.0, pixel_height: -60.0 };
            let raster =
                Arc::new(Raster::new("heights", 10, 3, vec![vec![0.0; 30]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap());
            Ground { heights: Heights::Raster { raster, wrap_width: None }, exaggeration: 1.0, body: 1.0, water: None }
        };
        let east_of_seam = regional(-180.0).cells_across(&[at(170.0, 30.0), at(186.0, 30.0)]).unwrap().0;
        assert!((east_of_seam - 6.0).abs() < 1e-6, "{east_of_seam}");
        let a_turn_on = regional(0.0).cells_across(&[at(345.0, 30.0), at(365.0, 30.0)]).unwrap().0;
        assert!((a_turn_on - 5.0).abs() < 1e-6, "{a_turn_on}");
        // Elevation tiles: their pixels at the zoom read; the whole world at zoom 2 is 1024.
        let tiles = Ground {
            heights: Heights::Tiles {
                url: String::new(),
                encoding: TerrainEncoding::Terrarium,
                zoom: 2,
                read: RefCell::new(HashMap::new()),
                pending: RefCell::new(false),
                waiting: RefCell::new(std::collections::HashSet::new()),
            },
            exaggeration: 1.0,
            body: 1.0,
            water: None,
        };
        let half = std::f64::consts::PI * MERCATOR_R;
        let world = tiles.cells_across(&[Coord::new(-2.0 * half, half), Coord::new(2.0 * half, -half)]).unwrap();
        assert!((world.0 - 1024.0).abs() < 1e-6 && (world.1 - 1024.0).abs() < 1e-6, "{world:?}");
        // An eighth of the world across, a sixteenth down.
        let quarter = tiles.cells_across(&[Coord::new(0.0, 0.0), Coord::new(half / 2.0, -half / 4.0)]).unwrap();
        assert!((quarter.0 - 256.0).abs() < 1e-6 && (quarter.1 - 128.0).abs() < 1e-6, "{quarter:?}");
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
