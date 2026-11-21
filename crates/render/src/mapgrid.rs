//! The lines of the map's measured, UTM and MGRS graticules (#1042), in longitude/latitude for the
//! graticule's drawing to project like its latitudes and longitudes. A measured grid is a layout's
//! measured grid (#1012) over the view; the UTM one draws the zones and latitude bands, and in each
//! zone in view its own eastings and northings. The MGRS one (NGA.STND.0037) draws the same zones
//! and bands as grid zone designations, each one's grid at a power of ten that suits the scale, and
//! names the grid zone designations or the 100 km squares in view.

use rustgis_geom::utm::{
    UTM_MAX_LAT, UTM_MIN_LAT, band_latitudes, from_utm, latitude_band, square_letters, to_utm_in_zone, utm_zone, zone_longitudes,
};
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

/// The side of an MGRS 100 km square, in meters: the MGRS grid's coarsest step.
const SQUARE: f64 = 100_000.0;
/// The MGRS grid's finest step, in meters.
const MGRS_FINEST: f64 = 100.0;
/// The most grid zone designations in view whose grid is drawn finer than 100 km: four meet at a
/// corner; past them the view is wide enough for 100 km lines.
const FEW_CELLS: usize = 4;
/// The size of the MGRS graticule's labels, in points (as a layout's MGRS square labels).
pub(crate) const LABEL_PT: f32 = 10.0;
/// A label's height in points: a line of [`LABEL_PT`] text.
const LABEL_H: f64 = LABEL_PT as f64 * 1.33 * 1.2;
/// The places across and down a 100 km square (and half as many across a grid zone designation)
/// whose middle on the screen places its label.
const LABEL_SAMPLES: u32 = 8;
/// The most 100 km squares of one grid zone designation looked at for names in one view.
const MAX_SQUARES: i64 = 400;

/// A graticule's lines in longitude/latitude, in two weights: the MGRS grid's grid zone
/// designations and 100 km lines are `major`, drawn heavier; every other line is `minor`.
#[derive(Debug, Default)]
pub(crate) struct GridLines {
    pub(crate) major: Vec<Vec<Coord>>,
    pub(crate) minor: Vec<Vec<Coord>>,
}

/// The graticule's lines in longitude/latitude for a measured, UTM or MGRS graticule; `None` for
/// latitudes and longitudes, which the graticule draws itself. They cover the view and the margin
/// around it the GPU moves a frame's meshes within while panning ([`crate::GPU_CULL_MARGIN`]), at
/// the step the view itself suits.
pub(crate) fn lines(map: &Map, view: &View) -> Option<GridLines> {
    if map.graticule_type == GraticuleType::Degrees {
        return None;
    }
    let globe = map.projection.is_globe().then(|| Globe::new(&view.camera, view.width, view.height));
    let (shown, around) = (in_view(map, view, globe.as_ref(), 0.0), in_view(map, view, globe.as_ref(), crate::GPU_CULL_MARGIN));
    let center = at(map, view, globe.as_ref(), view.width / 2.0, view.height / 2.0).or_else(|| shown.get(shown.len() / 2).copied());
    let Some(center) = center else { return Some(GridLines::default()) };
    Some(match map.graticule_type {
        GraticuleType::Degrees => GridLines::default(),
        GraticuleType::Measured => GridLines { minor: measured(map, center, &shown, &around), major: Vec::new() },
        GraticuleType::Utm => zones(center, &shown, &around, false),
        GraticuleType::Mgrs => zones(center, &shown, &around, true),
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

/// How far the places `shown` reach across or down, the more of the two, in `tr`'s system.
fn span_over(tr: &rustgis_data::raster::Transformer, shown: &[Coord]) -> Option<f64> {
    let mut m = Extent::EMPTY;
    for c in shown.iter().filter_map(|c| tr.from_lonlat(*c)) {
        m.expand(c);
    }
    (!m.is_empty() && m.width().is_finite() && m.height().is_finite()).then(|| m.width().max(m.height()))
}

/// The step that draws about eight lines across the places `shown`, in `tr`'s system.
fn step_over(tr: &rustgis_data::raster::Transformer, shown: &[Coord]) -> Option<f64> {
    span_over(tr, shown).map(|span| nice_step(span, rustgis_map::layout::MIN_MEASURED_STEP))
}

/// The MGRS grid's step for a view `span` meters across (the more of across and down) showing
/// `cells` grid zone designations: the power of ten, from 100 m to 100 km, that draws between
/// three and thirty lines across it (NGA.STND.0037 asks for a power of ten that suits the scale);
/// 100 km while more than [`FEW_CELLS`] grid zone designations show.
fn mgrs_step(span: f64, cells: usize) -> f64 {
    if cells > FEW_CELLS {
        return SQUARE;
    }
    let (finest, coarsest) = (MGRS_FINEST.log10(), SQUARE.log10());
    let power = (span / 3.0).log10().floor();
    // (A view of no width, or a span that isn't a number: the finest step.)
    let power = if power.is_nan() { finest } else { power.clamp(finest, coarsest) };
    10f64.powi(power as i32)
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

/// Every UTM zone-and-band cell (grid zone designation): its zone, band, and longitudes and
/// latitudes, with the Norway and Svalbard exceptions.
fn cells() -> impl Iterator<Item = (u8, char, Extent)> {
    BANDS.chars().flat_map(|band| {
        (1..=60u8).filter_map(move |zone| {
            let (south, north) = band_latitudes(band)?;
            let (west, east) = zone_longitudes(zone, band)?;
            Some((zone, band, Extent { min_x: west, min_y: south, max_x: east, max_y: north }))
        })
    })
}

/// The UTM graticule's lines (all of them minor; see [`zones`]).
#[cfg(test)]
fn utm(center: Coord, shown: &[Coord], around: &[Coord]) -> Vec<Vec<Coord>> {
    zones(center, shown, around, false).minor
}

/// The UTM zones and latitude bands in view and around it, and in each of a few zones in view its
/// own grid, one step apart in all of them, cut at the zone's edges. For UTM (`mgrs` false) the
/// step is the one the view suits in the zone at its center, and every line is minor; for MGRS a
/// power of ten ([`mgrs_step`]), with the zones' edges and the 100 km lines major.
fn zones(center: Coord, shown: &[Coord], around: &[Coord], mgrs: bool) -> GridLines {
    let mut out = GridLines::default();
    let parts = extents(around);
    if parts.is_empty() {
        return out;
    }
    let has_lon =
        |lon: f64, south: f64, north: f64| parts.iter().any(|e| e.min_x <= lon && lon <= e.max_x && e.min_y <= north && south <= e.max_y);
    let edges = if mgrs { &mut out.major } else { &mut out.minor };
    // (Made once a frame: the map redraws often.)
    let all: Vec<(u8, char, Extent)> = cells().collect();
    // Each zone's western edge within each band, and the last zone's eastern one at 180°, so the
    // Norway and Svalbard zones are drawn as they are. (`cells` gives them band by band.)
    for band_cells in all.chunk_by(|a, b| a.1 == b.1) {
        let Some((south, north)) = band_cells.first().map(|(_, _, c)| (c.min_y, c.max_y)) else { continue };
        for (_, _, cell) in band_cells {
            if has_lon(cell.min_x, south, north) {
                edges.push(meridian(cell.min_x, south, north));
            }
        }
        if has_lon(180.0, south, north) {
            edges.push(meridian(180.0, south, north));
        }
    }
    // The bands' edges, and the zones' southern and northern limits.
    let lats = BANDS.chars().filter_map(|b| band_latitudes(b).map(|(south, _)| south)).chain([UTM_MAX_LAT]);
    for lat in lats {
        for e in parts.iter().filter(|e| e.min_y <= lat && lat <= e.max_y) {
            edges.push(parallel(lat, e.min_x.max(-180.0), e.max_x.min(180.0)));
        }
    }
    // Eastings and northings when few zone-and-band cells are in the view itself (not counting
    // the margin, so they don't come and go as it pans), drawn over the margin too.
    let seen_parts = extents(shown);
    let seen_cells = all.iter().filter(|(_, _, c)| seen_parts.iter().any(|e| overlaps(c, e))).count();
    if seen_cells == 0 || seen_cells > MAX_GRIDDED_CELLS {
        return out;
    }
    let in_view: Vec<_> = all.iter().filter_map(|&(z, b, c)| parts.iter().find(|e| overlaps(&c, e)).map(|e| (z, b, c, *e))).collect();
    let step = zone_transformer(center)
        .and_then(|tr| if mgrs { span_over(&tr, shown).map(|span| mgrs_step(span, seen_cells)) } else { step_over(&tr, shown) });
    let Some(step) = step else { return out };
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
        let frame = frame_of(&part);
        let g = Grid { kind: GridKind::Measured, step, labels: false, ..Grid::default() };
        let mut zone_lines = Vec::new();
        measured_lines(&g, &tr, &frame, false, &mut zone_lines);
        for (pts, _, easting) in zone_lines {
            // An MGRS line at a whole 100 km (its first point's easting or northing) bounds squares.
            let value = pts.first().and_then(|c| tr.from_lonlat(*c)).map(|p| if easting { p.x } else { p.y });
            let major = mgrs && value.is_some_and(|v| (v - (v / SQUARE).round() * SQUARE).abs() < 1.0);
            let to = if major { &mut out.major } else { &mut out.minor };
            to.extend(clip(&pts, &cell));
        }
    }
    out
}

/// The MGRS graticule's labels on `view` (text, and its center in the view's points), none for
/// another type. Zoomed out, or while the 100 km squares are too small on the screen to name, each
/// grid zone designation in view is named (`33U`) at the middle of what shows of it; zoomed in,
/// each 100 km square of each one, by its letters (`VP`), with its grid zone designation (`33U VP`)
/// when more than one is in view. A label goes only where its corners, edge middles and center all
/// stay within its zone and square and the view (a sample, [`label_fits`]), so a zone or square cut
/// too small isn't named. Nothing is named in the polar
/// areas (UPS). `width` measures a text across in points at [`LABEL_PT`].
pub(crate) fn labels(map: &Map, view: &View, width: impl Fn(&str) -> f32) -> Vec<(String, f32, f32)> {
    if map.graticule_type != GraticuleType::Mgrs {
        return Vec::new();
    }
    let globe = map.projection.is_globe().then(|| Globe::new(&view.camera, view.width, view.height));
    let globe = globe.as_ref();
    let (w, h) = (view.width, view.height);
    let on_view = |(x, y): (f64, f64)| (0.0..=w).contains(&x) && (0.0..=h).contains(&y);
    // Where a place is drawn, on the view or off it (on a globe, only on its near side).
    let project = |c: Coord| -> Option<(f64, f64)> {
        let s = match globe {
            Some(g) => g.forward(c)?,
            None => view.to_screen(map.to_map(c)?),
        };
        (s.0.is_finite() && s.1.is_finite()).then_some(s)
    };
    // Where a place is on the view, if it is on it.
    let screen = |c: Coord| project(c).filter(|s| on_view(*s));
    let place = |x: f64, y: f64| at(map, view, globe, x, y);
    let shown = in_view(map, view, globe, 0.0);
    let parts = extents(&shown);
    let seen: Vec<(u8, char, Extent)> = cells().filter(|(_, _, c)| parts.iter().any(|e| overlaps(c, e))).collect();
    if seen.is_empty() {
        return Vec::new();
    }
    // A 100 km square's side on the screen, from a kilometer's, at the view's center or else the
    // first place in view in a grid zone designation (a view centered past 84° N still shows
    // band X).
    let square_pt = place(w / 2.0, h / 2.0).into_iter().chain(shown.iter().copied()).find_map(|c| {
        let &(zone, band, _) = seen.iter().find(|(_, _, cell)| cell.contains(c))?;
        let north = band >= 'N';
        let (e, n) = to_utm_in_zone(c, zone, north)?;
        let (sx, sy) = screen(c)?;
        let side = |p: Option<Coord>| p.and_then(project).map(|(x, y)| (x - sx).hypot(y - sy) * SQUARE / 1000.0);
        Some(side(from_utm(zone, north, e + 1000.0, n))?.min(side(from_utm(zone, north, e, n + 1000.0))?))
    });
    // Squares named once the 100 km lines are drawn and a square is three labels tall.
    let squares = seen.len() <= MAX_GRIDDED_CELLS && square_pt.is_some_and(|s| s >= 3.0 * LABEL_H);
    // With more than one grid zone designation reaching into the view, a square's name carries
    // its own: every named square is in one of the `seen` cells.
    let one_gzd = seen.len() <= 1;
    let mut out: Vec<(String, f32, f32)> = Vec::new();
    let mut named = std::collections::BTreeSet::new();
    for (zone, band, cell) in seen {
        let north = band >= 'N';
        let gzd = format!("{zone:02}{band}");
        let in_cell = |c: Coord| utm_zone(c) == Some(zone) && latitude_band(c.y) == Some(band);
        for e in parts.iter().filter(|e| overlaps(&cell, e)) {
            let part = Extent {
                min_x: cell.min_x.max(e.min_x),
                min_y: cell.min_y.max(e.min_y),
                max_x: cell.max_x.min(e.max_x),
                max_y: cell.max_y.min(e.max_y),
            };
            if !squares {
                let samples = grid_of(LABEL_SAMPLES / 2, part.min_x, part.min_y, part.max_x, part.max_y).filter(|c| in_cell(*c));
                let Some((x, y)) = middle(samples.filter_map(&screen)) else { continue };
                let fits = |c: Coord| in_cell(c);
                if !named.contains(&(zone, band, 0, 0)) && label_fits(&gzd, (x, y), &width, on_view, place, fits) {
                    named.insert((zone, band, 0, 0));
                    out.push((gzd.clone(), x as f32, y as f32));
                }
                continue;
            }
            // The squares the part reaches, by its edges' eastings and northings in the zone.
            let mut m = Extent::EMPTY;
            for c in frame_of(&part) {
                if let Some((x, y)) = to_utm_in_zone(c, zone, north) {
                    m.expand(Coord::new(x, y));
                }
            }
            if m.is_empty() {
                continue;
            }
            let (pad_x, pad_y) = (m.width() * 0.01, m.height() * 0.01);
            let (e0, e1) = (((m.min_x - pad_x) / SQUARE).floor() as i64, ((m.max_x + pad_x) / SQUARE).floor() as i64);
            let (n0, n1) = (((m.min_y - pad_y) / SQUARE).floor() as i64, ((m.max_y + pad_y) / SQUARE).floor() as i64);
            if (e1 - e0 + 1).saturating_mul(n1 - n0 + 1) > MAX_SQUARES {
                continue;
            }
            for (ie, inn) in (e0..=e1).flat_map(|ie| (n0..=n1).map(move |inn| (ie, inn))) {
                let (se, sn) = (ie as f64 * SQUARE, inn as f64 * SQUARE);
                let Some((column, row)) = square_letters(zone, se + SQUARE / 2.0, sn + SQUARE / 2.0) else { continue };
                // What of the square the part reaches, sampled in the zone's grid.
                let (x0, x1) = (se.max(m.min_x - pad_x), (se + SQUARE).min(m.max_x + pad_x));
                let (y0, y1) = (sn.max(m.min_y - pad_y), (sn + SQUARE).min(m.max_y + pad_y));
                if x0 >= x1 || y0 >= y1 {
                    continue;
                }
                let samples =
                    grid_of(LABEL_SAMPLES, x0, y0, x1, y1).filter_map(|p| from_utm(zone, north, p.x, p.y)).filter(|c| in_cell(*c));
                let Some((x, y)) = middle(samples.filter_map(&screen)) else { continue };
                let text = if one_gzd { format!("{column}{row}") } else { format!("{gzd} {column}{row}") };
                let fits = |c: Coord| {
                    in_cell(c)
                        && to_utm_in_zone(c, zone, north)
                            .is_some_and(|(x, y)| (x / SQUARE).floor() as i64 == ie && (y / SQUARE).floor() as i64 == inn)
                };
                if !named.contains(&(zone, band, ie, inn)) && label_fits(&text, (x, y), &width, on_view, place, fits) {
                    named.insert((zone, band, ie, inn));
                    out.push((text, x as f32, y as f32));
                }
            }
        }
    }
    out
}

/// `n` places across and down a box, at the middles of its cells.
fn grid_of(n: u32, x0: f64, y0: f64, x1: f64, y1: f64) -> impl Iterator<Item = Coord> {
    (0..n).flat_map(move |i| {
        (0..n).map(move |j| {
            let (ti, tj) = ((f64::from(i) + 0.5) / f64::from(n), (f64::from(j) + 0.5) / f64::from(n));
            Coord::new(x0 + (x1 - x0) * ti, y0 + (y1 - y0) * tj)
        })
    })
}

/// A longitude/latitude box's edges, eight points a side.
fn frame_of(b: &Extent) -> Vec<Coord> {
    (0..=8)
        .flat_map(|i| {
            let t = f64::from(i) / 8.0;
            let (x, y) = (b.min_x + b.width() * t, b.min_y + b.height() * t);
            [Coord::new(x, b.min_y), Coord::new(x, b.max_y), Coord::new(b.min_x, y), Coord::new(b.max_x, y)]
        })
        .collect()
}

/// The middle of points on the screen; `None` for none.
fn middle(points: impl Iterator<Item = (f64, f64)>) -> Option<(f64, f64)> {
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0u32);
    for (x, y) in points {
        (sx, sy, n) = (sx + x, sy + y, n + 1);
    }
    (n > 0).then(|| (sx / f64::from(n), sy / f64::from(n)))
}

/// Whether a label `text` centered at `(x, y)` (points), with a little room around it, lies on the
/// view (`on_view`) and over the place it names (`fits`, for the places under its corners, its
/// edges' middles and its center, as `place` finds them).
fn label_fits(
    text: &str,
    (x, y): (f64, f64),
    width: impl Fn(&str) -> f32,
    on_view: impl Fn((f64, f64)) -> bool,
    place: impl Fn(f64, f64) -> Option<Coord>,
    fits: impl Fn(Coord) -> bool,
) -> bool {
    let (hw, hh) = label_reach(width(text));
    [(-1.0, -1.0), (0.0, -1.0), (1.0, -1.0), (-1.0, 0.0), (0.0, 0.0), (1.0, 0.0), (-1.0, 1.0), (0.0, 1.0), (1.0, 1.0)].iter().all(
        |&(kx, ky)| {
            let p = (x + kx * hw, y + ky * hh);
            on_view(p) && place(p.0, p.1).is_some_and(&fits)
        },
    )
}

/// How far a label `width` points across reaches from its center, across and down (points), with
/// a little room around it.
pub(crate) fn label_reach(width: f32) -> (f64, f64) {
    (f64::from(width) / 2.0 + 2.0, LABEL_H / 2.0 + 2.0)
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
        let own = lines(&m, &view).unwrap().minor;
        assert!(straight_in(&own, 2056), "{own:?}");
        m.graticule_crs = Some("EPSG:32632".into());
        let utm = lines(&m, &view).unwrap().minor;
        assert!(straight_in(&utm, 32632) && !straight_in(&utm, 2056));
        // On a Web Mercator map with no system given: the UTM zone at the view's centre (32N).
        let (mut m, _) = winterthur("EPSG:3857");
        m.projection = rustgis_map::Projection::WebMercator;
        let center = m.to_map(Coord::new(8.73, 47.5)).unwrap();
        let view = View { camera: rustgis_map::camera::Camera::new(center, 10.0), ..view };
        m.graticule_type = GraticuleType::Measured;
        assert!(straight_in(&lines(&m, &view).unwrap().minor, 32632));
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
        for l in lines(&m, &view).unwrap().minor {
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

    /// Places over a longitude/latitude box, 13 a side.
    fn over(w: f64, s: f64, e: f64, n: f64) -> Vec<Coord> {
        (0..=12)
            .flat_map(|i| (0..=12).map(move |j| Coord::new(w + (e - w) * f64::from(i) / 12.0, s + (n - s) * f64::from(j) / 12.0)))
            .collect()
    }

    /// A Web Mercator map with the MGRS graticule, its view 800 × 600 points centered on `lon`,
    /// `lat` at `res` Web Mercator meters a point.
    fn mgrs_view(lon: f64, lat: f64, res: f64) -> (Map, View) {
        let mut m = Map::new("m");
        m.graticule_type = GraticuleType::Mgrs;
        let center = m.to_map(Coord::new(lon, lat)).unwrap();
        let camera = rustgis_map::camera::Camera::new(center, res);
        (m, View { width: 800.0, height: 600.0, pixels_per_point: 1.0, camera, sky: false })
    }

    /// The MGRS labels on a view, with each character 6 points across.
    fn labels_on(m: &Map, view: &View) -> Vec<(String, f32, f32)> {
        labels(m, view, |t| t.chars().count() as f32 * 6.0)
    }

    /// Whether each label names the place under its center as `to_mgrs` does (`name` writes it).
    fn name_their_places(m: &Map, view: &View, labels: &[(String, f32, f32)], name: impl Fn(&rustgis_geom::Mgrs) -> String) {
        let globe = m.projection.is_globe().then(|| Globe::new(&view.camera, view.width, view.height));
        for (text, x, y) in labels {
            let c = at(m, view, globe.as_ref(), f64::from(*x), f64::from(*y)).unwrap();
            let r = rustgis_geom::to_mgrs(c, 0).unwrap();
            assert_eq!(text, &name(&r), "at {c:?}");
        }
    }

    fn gzd(r: &rustgis_geom::Mgrs) -> String {
        format!("{:02}{}", r.zone, r.band)
    }

    /// A line's easting (`true`) or northing in `tr`'s system, when it keeps one.
    fn grid_value(l: &[Coord], tr: &rustgis_data::raster::Transformer) -> Option<(bool, f64)> {
        let pts: Vec<Coord> = l.iter().filter_map(|c| tr.from_lonlat(*c)).collect();
        let first = pts.first()?;
        if pts.iter().all(|p| (p.x - first.x).abs() < 0.01) {
            Some((true, first.x))
        } else if pts.iter().all(|p| (p.y - first.y).abs() < 0.01) {
            Some((false, first.y))
        } else {
            None
        }
    }

    /// The MGRS grid's step is a power of ten, three to thirty lines across the view, from 100 km
    /// to 100 m, and 100 km while more than four grid zone designations show.
    #[test]
    fn the_mgrs_step_is_a_power_of_ten_by_scale() {
        for (span, cells, step) in [
            (2e6, 2, 1e5),
            (4e5, 1, 1e5),
            (2e5, 1, 1e4),
            (3.1e4, 1, 1e4),
            (2.9e4, 1, 1e3),
            (3.1e3, 4, 1e3),
            (2.9e3, 1, 100.0),
            (50.0, 1, 100.0),
            (0.0, 1, 100.0),
            (f64::NAN, 1, 100.0),
            (f64::INFINITY, 1, 1e5),
            (3.1e3, 5, 1e5),
        ] {
            assert_eq!(mgrs_step(span, cells), step, "{span} m, {cells} cells");
        }
    }

    /// Zoomed in on zone 33U, the MGRS grid draws 100 km, 10 km, 1 km, then 100 m lines (finest),
    /// the 100 km ones (the central meridian's 500 km easting among them) major, the finer minor;
    /// zoomed out over several zones, 100 km lines only, all major.
    #[test]
    fn the_mgrs_grid_steps_by_scale_with_its_100_km_lines_major() {
        let tr = rustgis_data::raster::Crs::from_epsg(32633).unwrap().transformer().unwrap();
        for (res, step) in [(100.0, 1e4), (10.0, 1e3), (1.0, 100.0), (0.1, 100.0)] {
            let (m, view) = mgrs_view(15.0, 49.5, res);
            let g = lines(&m, &view).unwrap();
            let minor: Vec<_> = g.minor.iter().filter_map(|l| grid_value(l, &tr)).collect();
            let major: Vec<_> = g.major.iter().filter_map(|l| grid_value(l, &tr)).collect();
            assert!(!minor.is_empty() && minor.len() == g.minor.len(), "{res}: {} of {}", minor.len(), g.minor.len());
            for (_, v) in &minor {
                let whole = |s: f64| (v / s - (v / s).round()).abs() < 1e-6;
                assert!(whole(step) && !whole(1e5), "{res}: {v} at {step}");
            }
            assert!(major.iter().any(|(e, v)| *e && (v - 500_000.0).abs() < 0.01), "{res}: {major:?}");
            assert!(major.iter().all(|(_, v)| (v / 1e5 - (v / 1e5).round()).abs() < 1e-6), "{res}: {major:?}");
        }
        // About 7° across: zones 32 to 34, so 100 km lines only.
        let (m, view) = mgrs_view(15.0, 49.5, 1000.0);
        let g = lines(&m, &view).unwrap();
        assert!(g.minor.is_empty() && g.major.len() > 20, "{} minor, {} major", g.minor.len(), g.major.len());
    }

    /// Other graticules draw no major lines and no labels.
    #[test]
    fn other_graticules_keep_one_weight_and_no_labels() {
        let (mut m, view) = mgrs_view(15.0, 49.5, 10.0);
        assert!(!labels_on(&m, &view).is_empty());
        for t in [GraticuleType::Measured, GraticuleType::Utm] {
            m.graticule_type = t;
            let g = lines(&m, &view).unwrap();
            assert!(g.major.is_empty() && !g.minor.is_empty(), "{t:?}");
            assert!(labels_on(&m, &view).is_empty(), "{t:?}");
        }
        m.graticule_type = GraticuleType::Degrees;
        assert!(lines(&m, &view).is_none() && labels_on(&m, &view).is_empty());
    }

    /// The MGRS grid keeps to each grid zone designation as UTM's does: near Salzburg each zone's
    /// lines stay on its side of 12° E; in southern Norway 32V reaches west of 6° E, so zone 32's
    /// lines cross 6° E; across the antimeridian zone 60's edge at 180° is drawn, major.
    #[test]
    fn mgrs_lines_keep_to_their_grid_zone_designation() {
        let seen = over(11.6, 47.3, 12.4, 47.7);
        let g = zones(Coord::new(12.0, 47.5), &seen, &seen, true);
        let all: Vec<_> = g.major.iter().chain(&g.minor).collect();
        for l in &all {
            assert!(l.iter().all(|c| c.x <= 12.0 + 1e-9) || l.iter().all(|c| c.x >= 12.0 - 1e-9), "across 12° E: {l:?}");
        }
        assert!(all.iter().any(|l| l.iter().all(|c| c.x < 12.0)) && all.iter().any(|l| l.iter().all(|c| c.x > 12.0)));
        // Southern Norway: no edge at 6° E in band V, and zone 32's grid across it.
        let seen = over(5.5, 59.8, 6.5, 60.2);
        let g = zones(Coord::new(6.0, 60.0), &seen, &seen, true);
        assert!(!g.major.iter().any(|l| l.iter().all(|c| (c.x - 6.0).abs() < 1e-9)));
        let tr = rustgis_data::raster::Crs::from_epsg(32632).unwrap().transformer().unwrap();
        let grid: Vec<_> = g.minor.iter().filter(|l| grid_value(l, &tr).is_some()).collect();
        assert!(grid.len() > 4 && grid.iter().any(|l| l.iter().any(|c| c.x < 6.0) && l.iter().any(|c| c.x > 6.0)), "{}", grid.len());
        // Svalbard's band X: its edges at 9°, 21° and 33° E, none at 12° E.
        let seen = over(0.0, 74.0, 40.0, 82.0);
        let g = zones(Coord::new(20.0, 78.0), &seen, &seen, true);
        let edge = |lon: f64| g.major.iter().any(|l| l.iter().all(|c| (c.x - lon).abs() < 1e-9) && l.iter().any(|c| c.y > 76.0));
        assert!(edge(9.0) && edge(21.0) && edge(33.0) && !edge(12.0));
        // Fiji, across the antimeridian.
        let seen: Vec<Coord> =
            over(179.4, -17.6, 180.6, -17.0).into_iter().map(|c| Coord::new(if c.x > 180.0 { c.x - 360.0 } else { c.x }, c.y)).collect();
        let g = zones(Coord::new(179.9, -17.3), &seen, &seen, true);
        assert!(g.major.iter().any(|l| l.iter().all(|c| (c.x - 180.0).abs() < 1e-9)));
        let inner = g.minor.iter().chain(&g.major).filter(|l| !l.iter().all(|c| (c.x - l[0].x).abs() < 1e-9)).count();
        assert!(inner > 4, "{inner}");
    }

    /// The 100 km squares are named by their letters at their places, checked against
    /// GeographicLib's GeoConvert (`GeoConvert -m -p 0`): 48.5° N 14.5° E is 33U VP, 47.8° N
    /// 14.5° E 33T VN, 52° N 15° E 33U WT. With two grid zone designations in view each name
    /// carries its own; with one, the letters alone.
    #[test]
    fn mgrs_squares_are_named_by_their_letters() {
        let (m, view) = mgrs_view(14.5, 48.2, 500.0);
        let got = labels_on(&m, &view);
        let texts: Vec<&str> = got.iter().map(|(t, _, _)| t.as_str()).collect();
        assert!(texts.contains(&"33U VP") && texts.contains(&"33T VN"), "{texts:?}");
        name_their_places(&m, &view, &got, |r| format!("{} {}{}", gzd(r), r.column, r.row));
        let (m, view) = mgrs_view(15.0, 52.0, 300.0);
        let got = labels_on(&m, &view);
        let texts: Vec<&str> = got.iter().map(|(t, _, _)| t.as_str()).collect();
        assert!(texts.contains(&"WT") && texts.iter().all(|t| t.len() == 2), "{texts:?}");
        name_their_places(&m, &view, &got, |r| format!("{}{}", r.column, r.row));
        // Zoomed in to a few hundred meters, within one square (52° N 15.3° E: 33U WT).
        let (m, view) = mgrs_view(15.3, 52.0, 0.5);
        let got = labels_on(&m, &view);
        assert_eq!(got.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>(), ["WT"]);
        let (_, x, y) = &got[0];
        assert!((f64::from(*x) - 400.0).abs() < 1.0 && (f64::from(*y) - 300.0).abs() < 1.0, "in the middle: {x}, {y}");
    }

    /// Zoomed out, the grid zone designations are named, not the squares, on a flat map and on a
    /// globe; across the antimeridian on a globe both zone 60's and zone 1's squares are named
    /// (60K ZF and 01K AA by GeoConvert at 17.3° S, 179.9° E and W).
    #[test]
    fn zoomed_out_mgrs_names_its_grid_zone_designations() {
        let (mut m, view) = mgrs_view(15.0, 48.0, 20_000.0);
        let got = labels_on(&m, &view);
        assert!(got.len() > 10 && got.iter().any(|(t, _, _)| t == "33U"), "{got:?}");
        name_their_places(&m, &view, &got, gzd);
        m.projection = rustgis_map::Projection::Globe;
        let camera = rustgis_map::camera::Camera::new(rustgis_geom::lonlat_to_merc(Coord::new(15.0, 48.0)), 20_000.0);
        let view = View { camera, ..view };
        let got = labels_on(&m, &view);
        assert!(got.len() > 10 && got.iter().any(|(t, _, _)| t == "33U"), "{got:?}");
        name_their_places(&m, &view, &got, gzd);
        let camera = rustgis_map::camera::Camera::new(rustgis_geom::lonlat_to_merc(Coord::new(179.9, -17.3)), 300.0);
        let view = View { camera, ..view };
        let got = labels_on(&m, &view);
        let texts: Vec<&str> = got.iter().map(|(t, _, _)| t.as_str()).collect();
        assert!(texts.contains(&"60K ZF") && texts.contains(&"01K AA"), "{texts:?}");
        name_their_places(&m, &view, &got, |r| format!("{} {}{}", gzd(r), r.column, r.row));
    }

    /// A square cut too small for its name isn't named: with five points of zone 34 at the view's
    /// right edge only zone 33's squares are, with two hundred zone 34's too.
    #[test]
    fn a_square_cut_too_small_is_not_named() {
        let per_pt = 50.0 / 111_319.490_793;
        let (m, view) = mgrs_view(18.0 - 395.0 * per_pt, 49.5, 50.0);
        let got = labels_on(&m, &view);
        assert!(!got.is_empty() && got.iter().all(|(t, _, _)| t.starts_with("33U ")), "{got:?}");
        let (m, view) = mgrs_view(18.0 - 200.0 * per_pt, 49.5, 50.0);
        let got = labels_on(&m, &view);
        assert!(got.iter().any(|(t, _, _)| t.starts_with("34U ")) && got.iter().any(|(t, _, _)| t.starts_with("33U ")), "{got:?}");
        name_their_places(&m, &view, &got, |r| format!("{} {}{}", gzd(r), r.column, r.row));
    }

    /// A view of 33U with a narrow strip of 34U at its right edge (north up and turned), or of
    /// 33T at its bottom, names its squares with their grid zone designations.
    #[test]
    fn a_narrow_strip_of_another_grid_zone_designation_prefixes_the_names() {
        let per_pt = 50.0 / 111_319.490_793;
        for bearing in [0.0, 30.0] {
            for (lon, lat) in [(18.0 - 360.0 * per_pt, 49.5), (15.0, 48.0 + 270.0 * per_pt * 0.66)] {
                let (m, mut view) = mgrs_view(lon, lat, 50.0);
                view.camera.bearing = bearing;
                let got = labels_on(&m, &view);
                assert!(!got.is_empty() && got.iter().all(|(t, _, _)| t.len() == 6), "{bearing}: {got:?}");
                name_their_places(&m, &view, &got, |r| format!("{} {}{}", gzd(r), r.column, r.row));
            }
        }
    }

    /// A view centered past 84° N, where no UTM zone is, names the band X squares it shows (the
    /// view reaches down to about 83.9° N).
    #[test]
    fn a_view_centered_past_84_north_names_band_x_squares() {
        let (m, view) = mgrs_view(15.0, 84.3, 1500.0);
        let got = labels_on(&m, &view);
        assert!(!got.is_empty() && got.iter().all(|(t, _, _)| t.len() == 2), "{got:?}");
        name_their_places(&m, &view, &got, |r| format!("{}{}", r.column, r.row));
    }
}
