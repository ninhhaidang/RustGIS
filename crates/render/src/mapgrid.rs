//! The lines of the map's measured and UTM graticules (#1042), in longitude/latitude for the
//! graticule's drawing to project like its latitudes and longitudes. A measured grid is a layout's
//! measured grid (#1012) over the view; the UTM one draws the zones and latitude bands, and in each
//! zone in view its own eastings and northings.

use rustgis_geom::utm::{UTM_MAX_LAT, UTM_MIN_LAT, band_latitudes, zone_longitudes};
use rustgis_geom::{Coord, Extent};
use rustgis_map::Map;
use rustgis_map::globe::Globe;
use rustgis_map::graticule::GraticuleType;
use rustgis_map::layout::{Grid, GridKind};

use crate::View;
use crate::layout::{measured_crs, measured_lines, nice_step};

/// The latitude bands, south to north (C to X, without I and O).
const BANDS: &str = "CDEFGHJKLMNPQRSTUVWX";

/// The most zone-and-band cells in view whose eastings and northings are drawn: past them (the
/// map zoomed out over many zones) only the zones and bands are.
const MAX_GRIDDED_CELLS: usize = 8;

/// The points a meridian or a parallel is drawn with, per degree.
const PER_DEGREE: f64 = 2.0;

/// The graticule's lines in longitude/latitude for a measured or UTM graticule; `None` for
/// latitudes and longitudes, which the graticule draws itself. They cover the view and the margin
/// around it the GPU moves a frame's meshes within while panning ([`crate::GPU_CULL_MARGIN`]), at
/// the step the view itself suits.
pub(crate) fn lines(map: &Map, view: &View) -> Option<Vec<Vec<Coord>>> {
    if map.graticule_type == GraticuleType::Degrees {
        return None;
    }
    let globe = map.projection.is_globe().then(|| Globe::new(&view.camera, view.width, view.height));
    let (shown, around) = (in_view(map, view, globe.as_ref(), 0.0), in_view(map, view, globe.as_ref(), crate::GPU_CULL_MARGIN));
    let center = at(map, view, globe.as_ref(), view.width / 2.0, view.height / 2.0).or_else(|| shown.get(shown.len() / 2).copied());
    let Some(center) = center else { return Some(Vec::new()) };
    Some(match map.graticule_type {
        GraticuleType::Degrees => Vec::new(),
        GraticuleType::Measured => measured(map, center, &shown, &around),
        GraticuleType::Utm => utm(center, &shown, &around),
    })
}

/// The place at a point of the view (in points), in longitude/latitude: through the globe when the
/// map is one, else through the map's flat projection.
pub(crate) fn at(map: &Map, view: &View, globe: Option<&Globe>, sx: f64, sy: f64) -> Option<Coord> {
    let c = match globe {
        Some(g) => g.inverse(sx, sy),
        None => map.to_lonlat(view.camera.to_map(sx, sy, view.width, view.height)),
    };
    c.filter(|c| c.is_finite())
}

/// Places in the view grown by `margin` of its size on every side, in longitude/latitude: a grid
/// of points over it (on a globe, those that fall on it), so a view whose corners miss the globe
/// still has its extent.
fn in_view(map: &Map, view: &View, globe: Option<&Globe>, margin: f64) -> Vec<Coord> {
    let (w, h) = (view.width, view.height);
    let n = 16;
    let mut out = Vec::with_capacity((n + 1) * (n + 1));
    for i in 0..=n {
        for j in 0..=n {
            let (tx, ty) = (-margin + (1.0 + 2.0 * margin) * i as f64 / n as f64, -margin + (1.0 + 2.0 * margin) * j as f64 / n as f64);
            out.extend(at(map, view, globe, w * tx, h * ty));
        }
    }
    out
}

/// The step that draws about eight lines across the places `shown`, in `tr`'s system.
fn step_over(tr: &rustgis_data::raster::Transformer, shown: &[Coord]) -> Option<f64> {
    let mut m = Extent::EMPTY;
    for c in shown.iter().filter_map(|c| tr.from_lonlat(*c)) {
        m.expand(c);
    }
    (!m.is_empty() && m.width().is_finite() && m.height().is_finite())
        .then(|| nice_step(m.width().max(m.height()), rustgis_map::layout::MIN_MEASURED_STEP))
}

/// A measured grid over the view and around it: the layout's measured grid in the map's graticule
/// system, at the step the view suits.
fn measured(map: &Map, center: Coord, shown: &[Coord], around: &[Coord]) -> Vec<Vec<Coord>> {
    let g = Grid { kind: GridKind::Measured, crs: map.graticule_crs.clone(), labels: false, ..Grid::default() };
    let Some(tr) = measured_crs(&g, map, center).and_then(|c| c.transformer().ok()) else { return Vec::new() };
    let Some(step) = step_over(&tr, shown) else { return Vec::new() };
    let mut lines = Vec::new();
    measured_lines(&Grid { step, ..g }, &tr, around, false, &mut lines);
    lines.into_iter().map(|(pts, _, _)| pts).filter(|l| l.len() >= 2).collect()
}

/// The longitude/latitude extents the places cover: one, or two either side of the antimeridian
/// when they lie across it (on a globe turned to 180°, longitudes jump from 180° to −180°).
fn extents(places: &[Coord]) -> Vec<Extent> {
    let mut e = Extent::EMPTY;
    for c in places {
        e.expand(*c);
    }
    if e.is_empty() {
        return Vec::new();
    }
    if e.width() > 180.0 {
        // The same places with the western hemisphere moved east of 180°: narrower when they lie
        // across the antimeridian.
        let mut east = Extent::EMPTY;
        for c in places {
            east.expand(Coord::new(if c.x < 0.0 { c.x + 360.0 } else { c.x }, c.y));
        }
        if east.width() < e.width() && east.max_x > 180.0 {
            return vec![Extent { max_x: 180.0, ..east }, Extent { min_x: -180.0, max_x: east.max_x - 360.0, ..east }];
        }
    }
    vec![e]
}

/// The UTM zones and latitude bands in view and around it, and in each of a few zones in view its
/// eastings and northings, one step apart in all of them (the step the view suits in the zone at
/// its centre).
fn utm(center: Coord, shown: &[Coord], around: &[Coord]) -> Vec<Vec<Coord>> {
    let parts = extents(around);
    if parts.is_empty() {
        return Vec::new();
    }
    let has_lon =
        |lon: f64, south: f64, north: f64| parts.iter().any(|e| e.min_x <= lon && lon <= e.max_x && e.min_y <= north && south <= e.max_y);
    let mut lines = Vec::new();
    // Each zone's western edge within each band, and the last zone's eastern one at 180°, so the
    // Norway and Svalbard zones are drawn as they are.
    let mut cells = Vec::new();
    for band in BANDS.chars() {
        let Some((south, north)) = band_latitudes(band) else { continue };
        for zone in 1..=60u8 {
            let Some((west, east)) = zone_longitudes(zone, band) else { continue };
            cells.push((zone, band, Extent { min_x: west, min_y: south, max_x: east, max_y: north }));
            if has_lon(west, south, north) {
                lines.push(meridian(west, south, north));
            }
        }
        if has_lon(180.0, south, north) {
            lines.push(meridian(180.0, south, north));
        }
    }
    // The bands' edges, and the zones' southern and northern limits.
    let edges = BANDS.chars().filter_map(|b| band_latitudes(b).map(|(south, _)| south)).chain([UTM_MAX_LAT]);
    for lat in edges {
        for e in parts.iter().filter(|e| e.min_y <= lat && lat <= e.max_y) {
            lines.push(parallel(lat, e.min_x.max(-180.0), e.max_x.min(180.0)));
        }
    }
    // Eastings and northings when few zone-and-band cells are in the view itself (not counting
    // the margin, so they don't come and go as it pans), drawn over the margin too.
    let seen_parts = extents(shown);
    let seen_cells = cells.iter().filter(|(_, _, c)| seen_parts.iter().any(|e| overlaps(c, e))).count();
    if seen_cells == 0 || seen_cells > MAX_GRIDDED_CELLS {
        return lines;
    }
    let in_view: Vec<_> = cells.into_iter().filter_map(|(z, b, c)| parts.iter().find(|e| overlaps(&c, e)).map(|e| (z, b, c, *e))).collect();
    let Some(step) = zone_transformer(center).and_then(|tr| step_over(&tr, shown)) else { return lines };
    for (zone, band, cell, e) in in_view {
        let north = band >= 'N';
        let epsg = if north { 32_600 } else { 32_700 } + u32::from(zone);
        // (Made once a code: the map redraws often.)
        let Some(tr) = rustgis_data::ogc::warp::transformer(epsg) else { continue };
        // The part of the cell in view, its edges densified, as the frame the grid covers.
        let part = Extent {
            min_x: cell.min_x.max(e.min_x),
            min_y: cell.min_y.max(e.min_y),
            max_x: cell.max_x.min(e.max_x),
            max_y: cell.max_y.min(e.max_y),
        };
        let frame: Vec<Coord> = (0..=8)
            .flat_map(|i| {
                let t = f64::from(i) / 8.0;
                let (x, y) = (part.min_x + part.width() * t, part.min_y + part.height() * t);
                [Coord::new(x, part.min_y), Coord::new(x, part.max_y), Coord::new(part.min_x, y), Coord::new(part.max_x, y)]
            })
            .collect();
        let g = Grid { kind: GridKind::Measured, step, labels: false, ..Grid::default() };
        let mut zone_lines = Vec::new();
        measured_lines(&g, &tr, &frame, false, &mut zone_lines);
        for (pts, _, _) in zone_lines {
            lines.extend(clip(&pts, &cell));
        }
    }
    lines
}

/// The transformer of the UTM zone at `c` (by its 6° zone and hemisphere), made once a code.
fn zone_transformer(c: Coord) -> Option<std::sync::Arc<rustgis_data::raster::Transformer>> {
    rustgis_data::ogc::warp::transformer(rustgis_map::layout::utm_epsg(c.x, c.y.clamp(UTM_MIN_LAT, UTM_MAX_LAT - 1e-9)))
}

fn overlaps(a: &Extent, b: &Extent) -> bool {
    a.min_x < b.max_x && b.min_x < a.max_x && a.min_y < b.max_y && b.min_y < a.max_y
}

fn meridian(lon: f64, south: f64, north: f64) -> Vec<Coord> {
    let n = (((north - south) * PER_DEGREE).ceil() as usize).max(1);
    (0..=n).map(|i| Coord::new(lon, south + (north - south) * i as f64 / n as f64)).collect()
}

fn parallel(lat: f64, west: f64, east: f64) -> Vec<Coord> {
    let n = (((east - west) * PER_DEGREE).ceil() as usize).max(1);
    (0..=n).map(|i| Coord::new(west + (east - west) * i as f64 / n as f64, lat)).collect()
}

/// The runs of a polyline within a box (longitude/latitude), each segment cut at the box's edges.
fn clip(pts: &[Coord], b: &Extent) -> Vec<Vec<Coord>> {
    let mut out = Vec::new();
    let mut run: Vec<Coord> = Vec::new();
    for pair in pts.windows(2) {
        let &[p, q] = pair else { continue };
        match clip_segment(p, q, b) {
            Some((a, z)) => {
                if run.last().is_none_or(|l| (l.x - a.x).abs() > 1e-12 || (l.y - a.y).abs() > 1e-12) {
                    if run.len() >= 2 {
                        out.push(std::mem::take(&mut run));
                    }
                    run.clear();
                    run.push(a);
                }
                run.push(z);
            }
            None => {
                if run.len() >= 2 {
                    out.push(std::mem::take(&mut run));
                }
                run.clear();
            }
        }
    }
    if run.len() >= 2 {
        out.push(run);
    }
    out
}

/// A segment's part within a box (Liang–Barsky); `None` when none of it is.
fn clip_segment(p: Coord, q: Coord, b: &Extent) -> Option<(Coord, Coord)> {
    let (dx, dy) = (q.x - p.x, q.y - p.y);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (den, num) in [(-dx, p.x - b.min_x), (dx, b.max_x - p.x), (-dy, p.y - b.min_y), (dy, b.max_y - p.y)] {
        if den == 0.0 {
            if num < 0.0 {
                return None;
            }
        } else {
            let t = num / den;
            if den < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return None;
            }
        }
    }
    Some((Coord::new(p.x + t0 * dx, p.y + t0 * dy), Coord::new(p.x + t1 * dx, p.y + t1 * dy)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(w: f64, s: f64, e: f64, n: f64) -> Extent {
        Extent { min_x: w, min_y: s, max_x: e, max_y: n }
    }

    /// A view of Winterthur, 10 m a point, on a map in `projection`.
    fn winterthur(projection: &str) -> (Map, View) {
        let mut m = Map::new("m");
        m.projection = rustgis_map::Projection::Custom(projection.into());
        let center = m.to_map(Coord::new(8.73, 47.5)).unwrap();
        let view =
            View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera: rustgis_map::camera::Camera::new(center, 10.0), sky: false };
        (m, view)
    }

    /// Whether every line keeps one easting or one northing in `epsg`, and some of each.
    fn straight_in(lines: &[Vec<Coord>], epsg: u32) -> bool {
        let tr = rustgis_data::raster::Crs::from_epsg(epsg).unwrap().transformer().unwrap();
        let (mut eastings, mut northings) = (0, 0);
        for l in lines {
            let pts: Vec<Coord> = l.iter().map(|c| tr.from_lonlat(*c).unwrap()).collect();
            if pts.iter().all(|p| (p.x - pts[0].x).abs() < 0.01) {
                eastings += 1;
            } else if pts.iter().all(|p| (p.y - pts[0].y).abs() < 0.01) {
                northings += 1;
            } else {
                return false;
            }
        }
        eastings > 2 && northings > 2
    }

    /// A measured graticule on a map in LV95 draws LV95's eastings and northings; given a system,
    /// that one's; latitudes and longitudes are the graticule's own.
    #[test]
    fn a_measured_graticule_draws_the_maps_own_system_or_the_one_given() {
        let (mut m, view) = winterthur("EPSG:2056");
        assert!(lines(&m, &view).is_none(), "latitudes and longitudes");
        m.graticule_type = GraticuleType::Measured;
        let own = lines(&m, &view).unwrap();
        assert!(straight_in(&own, 2056), "{own:?}");
        m.graticule_crs = Some("EPSG:32632".into());
        let utm = lines(&m, &view).unwrap();
        assert!(straight_in(&utm, 32632) && !straight_in(&utm, 2056));
        // On a Web Mercator map with no system given: the UTM zone at the view's centre (32N).
        let (mut m, _) = winterthur("EPSG:3857");
        m.projection = rustgis_map::Projection::WebMercator;
        let center = m.to_map(Coord::new(8.73, 47.5)).unwrap();
        let view = View { camera: rustgis_map::camera::Camera::new(center, 10.0), ..view };
        m.graticule_type = GraticuleType::Measured;
        assert!(straight_in(&lines(&m, &view).unwrap(), 32632));
    }

    /// The grid covers the margin around the view that the GPU pans a frame within, so panning
    /// shows no edge; its step is the view's own (about eight lines across it), not the margin's.
    #[test]
    fn a_measured_graticule_covers_the_margin_at_the_views_step() {
        let (mut m, view) = winterthur("EPSG:2056");
        // 7.5 m a point: about 6,000 m across, so 1 km lines (eight-odd across the view), where
        // the view and its margin, about 12,000 m, would take 2 km.
        let view = View { camera: rustgis_map::camera::Camera { res: 7.5, ..view.camera }, ..view };
        m.graticule_type = GraticuleType::Measured;
        let tr = rustgis_data::raster::Crs::from_epsg(2056).unwrap().transformer().unwrap();
        let mut seen = Extent::EMPTY;
        for c in in_view(&m, &view, None, 0.0) {
            seen.expand(tr.from_lonlat(c).unwrap());
        }
        let mut drawn = Extent::EMPTY;
        let mut eastings = Vec::new();
        for l in lines(&m, &view).unwrap() {
            let pts: Vec<Coord> = l.iter().map(|c| tr.from_lonlat(*c).unwrap()).collect();
            pts.iter().for_each(|p| drawn.expand(*p));
            if pts.iter().all(|p| (p.x - pts[0].x).abs() < 0.01) {
                eastings.push(pts[0].x);
            }
        }
        // 1 km apart, over the view and half a view either side.
        assert!(drawn.min_x < seen.min_x - 0.4 * seen.width() && drawn.max_x > seen.max_x + 0.4 * seen.width(), "{drawn:?} {seen:?}");
        eastings.sort_by(f64::total_cmp);
        assert!(eastings.windows(2).all(|w| (w[1] - w[0] - 1000.0).abs() < 0.01), "{eastings:?}");
    }

    /// A view across the antimeridian (a globe turned to Fiji) is two extents, not one round the
    /// world: the zones in it get their eastings and northings, and the zones' edge at 180° is
    /// drawn, as it is at the right-hand edge of a flat world.
    #[test]
    fn utm_zones_across_the_antimeridian_keep_their_grids() {
        let seen: Vec<Coord> = (0..=10)
            .flat_map(|i| (0..=10).map(move |j| (f64::from(i), f64::from(j))))
            .map(|(i, j)| {
                let lon = 179.4 + 1.2 * i / 10.0;
                Coord::new(if lon > 180.0 { lon - 360.0 } else { lon }, -17.0 - 0.6 * j / 10.0)
            })
            .collect();
        let parts = extents(&seen);
        assert_eq!(parts.len(), 2, "{parts:?}");
        let lines = utm(Coord::new(179.9, -17.3), &seen, &seen);
        let at_180 = lines.iter().any(|l| l.iter().all(|c| (c.x - 180.0).abs() < 1e-9));
        let inner = lines.iter().filter(|l| !l.iter().all(|c| (c.x - l[0].x).abs() < 1e-9)).count();
        assert!(at_180 && inner > 4, "edge at 180°: {at_180}, eastings and northings: {inner}");
        // The whole flat world: the right-hand edge too.
        let world: Vec<Coord> =
            (0..=20).flat_map(|i| (0..=10).map(move |j| Coord::new(-180.0 + 18.0 * f64::from(i), -80.0 + 16.0 * f64::from(j)))).collect();
        let lines = utm(Coord::new(0.0, 0.0), &world, &world);
        assert!(lines.iter().any(|l| l.iter().all(|c| (c.x - 180.0).abs() < 1e-9)));
        assert!(lines.iter().any(|l| l.iter().all(|c| (c.x + 180.0).abs() < 1e-9)));
    }

    #[test]
    fn a_polyline_is_cut_at_a_box() {
        let b = bx(0.0, 0.0, 10.0, 10.0);
        // Across the box: the part inside.
        let runs = clip(&[Coord::new(-5.0, 5.0), Coord::new(15.0, 5.0)], &b);
        assert_eq!(runs, vec![vec![Coord::new(0.0, 5.0), Coord::new(10.0, 5.0)]]);
        // Out, in, out and in again: two runs.
        let runs = clip(&[Coord::new(5.0, -5.0), Coord::new(5.0, 5.0), Coord::new(5.0, 15.0), Coord::new(6.0, 5.0)], &b);
        assert_eq!(runs.len(), 2, "{runs:?}");
        // Wholly outside: nothing.
        assert!(clip(&[Coord::new(20.0, 20.0), Coord::new(30.0, 30.0)], &b).is_empty());
    }

    /// The eight-cell limit counts the view, not its margin: 8° to 20° E and 47° to 49° N holds six
    /// cells (32T to 34U), its margin ten, and its zones keep their eastings and northings.
    #[test]
    fn the_utm_grid_limit_counts_the_view_not_its_margin() {
        let grid = |w: f64, s: f64, e: f64, n: f64| -> Vec<Coord> {
            (0..=12)
                .flat_map(|i| (0..=12).map(move |j| Coord::new(w + (e - w) * f64::from(i) / 12.0, s + (n - s) * f64::from(j) / 12.0)))
                .collect()
        };
        let (shown, around) = (grid(8.0, 47.0, 20.0, 49.0), grid(2.0, 46.0, 26.0, 50.0));
        let lines = utm(Coord::new(14.0, 48.0), &shown, &around);
        let inner = lines
            .iter()
            .filter(|l| !l.iter().all(|c| (c.x - l[0].x).abs() < 1e-9) && !l.iter().all(|c| (c.y - l[0].y).abs() < 1e-9))
            .count();
        assert!(inner > 4, "eastings and northings: {inner}");
        // And they reach into the margin (zone 31, west of 6° E).
        assert!(lines.iter().any(|l| l.iter().all(|c| c.x < 6.0) && !l.iter().all(|c| (c.x - l[0].x).abs() < 1e-9)));
    }

    /// The UTM graticule over Scandinavia draws the zones' edges as they are there: southern
    /// Norway's 32V from 3° E to 12° E (no line at 6° E in band V), and in Svalbard's band X the
    /// edges at 9°, 21° and 33° E.
    #[test]
    fn utm_zones_keep_the_norway_and_svalbard_exceptions() {
        let seen: Vec<Coord> = (0..=20)
            .flat_map(|i| (0..=20).map(move |j| Coord::new(-2.0 + 46.0 * f64::from(i) / 20.0, 54.0 + 30.0 * f64::from(j) / 20.0)))
            .collect();
        let lines = utm(Coord::new(21.0, 69.0), &seen, &seen);
        // Meridians: a line whose points share a longitude, from its southern to northern end.
        let meridian_at = |lon: f64, lat: f64| {
            lines.iter().any(|l| l.len() >= 2 && l.iter().all(|c| (c.x - lon).abs() < 1e-9) && l[0].y <= lat && lat <= l[l.len() - 1].y)
        };
        // Band V (56° to 64° N).
        assert!(meridian_at(3.0, 60.0) && meridian_at(12.0, 60.0) && !meridian_at(6.0, 60.0));
        // Band W (64° to 72° N): the ordinary 6° zones.
        assert!(meridian_at(6.0, 68.0) && meridian_at(12.0, 68.0));
        // Band X (72° to 84° N).
        assert!(meridian_at(9.0, 78.0) && meridian_at(21.0, 78.0) && meridian_at(33.0, 78.0));
        assert!(!meridian_at(12.0, 78.0) && !meridian_at(18.0, 78.0));
        // Many zones in view: zones and bands only, no eastings or northings.
        assert!(lines.iter().all(|l| l.iter().all(|c| (c.x - l[0].x).abs() < 1e-9) || l.iter().all(|c| (c.y - l[0].y).abs() < 1e-9)));
    }

    /// Zoomed in on the edge between zones 32T (6° to 12° E) and 33T (12° to 18° E), near
    /// Salzburg: each zone's grid stays within it, and both are drawn.
    #[test]
    fn a_zones_grid_stays_within_it() {
        // Around 12° E, 47.5° N: zones 32T (6°–12° E) and 33T (12°–18° E).
        let seen: Vec<Coord> = (0..=10)
            .flat_map(|i| (0..=10).map(move |j| Coord::new(11.6 + 0.8 * f64::from(i) / 10.0, 47.3 + 0.4 * f64::from(j) / 10.0)))
            .collect();
        let lines = utm(Coord::new(12.0, 47.5), &seen, &seen);
        let inner: Vec<_> = lines.iter().filter(|l| !(l.iter().all(|c| (c.x - l[0].x).abs() < 1e-9))).collect();
        assert!(inner.len() > 4, "eastings and northings drawn: {}", inner.len());
        // No line of either zone's grid crosses 12° E.
        for l in &inner {
            let west = l.iter().all(|c| c.x <= 12.0 + 1e-9);
            let east = l.iter().all(|c| c.x >= 12.0 - 1e-9);
            assert!(west || east, "a line across the zones' edge: {l:?}");
        }
        // Lines on both sides.
        assert!(inner.iter().any(|l| l.iter().all(|c| c.x < 12.0)) && inner.iter().any(|l| l.iter().all(|c| c.x > 12.0)));
    }
}
