//! Tissot's indicatrix (#393): the same small circle on the Earth, repeated over a grid, shows on
//! the map how its projection stretches scale, area and angles.
//!
//! The circles are geodesic, in longitude/latitude, so the map draws each as the ellipse its
//! projection makes of it, and redraws them when the projection changes. When the map is flat, each
//! circle also carries Tissot's parameters at its centre for the map's projection at the time the
//! tool ran, from finite differences of the projection (Snyder, *Map Projections: A Working
//! Manual*, 1987, pp. 20–24).

use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value};
use rustgis_geom::{Coord, EARTH_RADIUS_M, Extent, geodesic_destination};
use serde_json::Value as Json;

use crate::{Inputs, Output, ToolResult};

/// Vertices per circle.
const VERTICES: usize = 72;
/// The most circles one run makes.
const MAX_CIRCLES: usize = 10_000;
/// How close to a pole a circle may reach (degrees of latitude): a longitude/latitude polygon
/// can't wrap around one.
const POLE_MARGIN: f64 = 0.5;
/// WGS 84, the datum of the longitudes and latitudes the projection is given.
const WGS84_A: f64 = 6_378_137.0;
const WGS84_E2: f64 = 0.006_694_379_990_141_317;

/// Tissot's parameters at a point: scale along the meridian (`h`) and the parallel (`k`), the
/// ellipse's semi-axes (`a` ≥ `b`), the area scale (`a·b`) and the largest angular distortion
/// (ω, degrees).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Distortion {
    pub h: f64,
    pub k: f64,
    pub a: f64,
    pub b: f64,
    pub area: f64,
    pub max_angle: f64,
}

/// Tissot's parameters of `project` (longitude/latitude to metres on the plane) at `c`, by
/// central differences on the WGS 84 ellipsoid. `None` where the projection doesn't reach both
/// sides of the point, or at a pole.
pub fn distortion(project: &dyn Fn(Coord) -> Option<Coord>, c: Coord) -> Option<Distortion> {
    let step = 1e-4;
    // A Map Extent grid may run past ±180°: measure at the same place on the Earth.
    let c = Coord::new((c.x + 180.0).rem_euclid(360.0) - 180.0, c.y);
    let at = |dx: f64, dy: f64| project(Coord::new(c.x + dx, c.y + dy));
    let (centre, east, west, north, south) = (at(0.0, 0.0)?, at(step, 0.0)?, at(-step, 0.0)?, at(0.0, step)?, at(0.0, -step)?);
    // Both sides of the centre must agree: one that falls on the far edge of the plane (the
    // antimeridian of a world projection, or past where it's cut) would make a huge difference.
    let agree = |a: Coord, b: Coord| {
        let (one, other) = ((a.x - centre.x, a.y - centre.y), (centre.x - b.x, centre.y - b.y));
        let gap = (one.0 - other.0).hypot(one.1 - other.1);
        gap <= 0.5 * one.0.hypot(one.1).max(other.0.hypot(other.1)) + 1e-9
    };
    if !agree(east, west) || !agree(north, south) {
        return None;
    }
    let d = 2.0 * step.to_radians();
    let (x_lam, y_lam) = ((east.x - west.x) / d, (east.y - west.y) / d);
    let (x_phi, y_phi) = ((north.x - south.x) / d, (north.y - south.y) / d);
    let phi = c.y.to_radians();
    let cos_phi = phi.cos();
    if cos_phi < 1e-9 {
        return None;
    }
    let w = (1.0 - WGS84_E2 * phi.sin().powi(2)).sqrt();
    let (n, m) = (WGS84_A / w, WGS84_A * (1.0 - WGS84_E2) / w.powi(3));
    let h = x_phi.hypot(y_phi) / m;
    let k = x_lam.hypot(y_lam) / (n * cos_phi);
    let area = ((y_phi * x_lam - x_phi * y_lam) / (m * n * cos_phi)).abs();
    let a1 = (h * h + k * k + 2.0 * area).sqrt();
    let b1 = (h * h + k * k - 2.0 * area).max(0.0).sqrt();
    let (a, b) = ((a1 + b1) / 2.0, (a1 - b1) / 2.0);
    let max_angle = 2.0 * ((a - b) / (a + b)).clamp(0.0, 1.0).asin().to_degrees();
    [h, k, a, b, area, max_angle].iter().all(|v| v.is_finite()).then_some(Distortion { h, k, a, b, area, max_angle })
}

/// A closed geodesic circle around `centre`, counter-clockwise in longitude/latitude.
pub fn circle(centre: Coord, metres: f64) -> Vec<Coord> {
    let mut ring: Vec<Coord> =
        (0..VERTICES).map(|i| geodesic_destination(centre, 360.0 - 360.0 * i as f64 / VERTICES as f64, metres)).collect();
    if let Some(&first) = ring.first() {
        ring.push(first);
    }
    ring
}

/// The circles' centres: every multiple of `spacing` degrees inside `area`, leaving out the
/// antimeridian when the area is the whole world and any circle that would reach a pole. Counted
/// before any is made: more than `cap` gives their number instead, so a fine spacing over a large
/// area can't run out of memory.
fn centres(area: Extent, spacing: f64, reach: f64, world: bool, cap: usize) -> Result<Vec<Coord>, u64> {
    // A view can reach past the Earth (or report nonsense): the grid stays on it, a turn of
    // longitude either side at most, so no count below can grow without bound or overflow.
    let (lon_min, lon_max) =
        if world { (-180.0 + spacing / 2.0, 180.0 - spacing / 2.0) } else { (area.min_x.max(-540.0), area.max_x.min(540.0)) };
    // The first and last multiple in [lo, hi] (none when last < first).
    let span = |lo: f64, hi: f64| ((lo / spacing).ceil() as i64, (hi / spacing).floor() as i64);
    let count = |(first, last): (i64, i64)| u64::try_from(last.saturating_sub(first).saturating_add(1)).unwrap_or(0);
    let lats: Vec<f64> = {
        let (first, last) = span(area.min_y.max(-90.0), area.max_y.min(90.0));
        // (At most 18,001 latitudes: a spacing is at least 0.01°.)
        (first..=last).map(|i| at(i, spacing)).filter(|lat| lat.abs() + reach < 90.0 - POLE_MARGIN).collect()
    };
    let mut lons = span(lon_min, lon_max);
    // A view a turn wide or more holds each place once (not at both `lon` and `lon + 360`): the
    // columns stop before the first that repeats one, which is a whole number of turns from the
    // first (12 columns at 30°, but 36 at 70°, whose multiples come back round only after 7
    // turns), keeping the view's own longitudes.
    if !world {
        let columns = lons.1.saturating_sub(lons.0).saturating_add(1).max(0);
        let repeats = |m: i64| {
            let turns = m as f64 * spacing / 360.0;
            (turns - turns.round()).abs() < 1e-9
        };
        if let Some(period) = (1..columns).find(|&m| repeats(m)) {
            lons.1 = lons.0 + period - 1;
        }
    }
    let n = (lats.len() as u64).saturating_mul(count(lons));
    if n > cap as u64 {
        return Err(n);
    }
    Ok(lats.iter().flat_map(|&lat| (lons.0..=lons.1).map(move |i| Coord::new(at(i, spacing), lat))).collect())
}

/// The `i`th multiple of `spacing`, without the binary noise of a spacing such as 0.1
/// (`0.30000000000000004`), so a centre's `lat` and `lon` read as typed.
fn at(i: i64, spacing: f64) -> f64 {
    (i as f64 * spacing * 1e9).round() / 1e9
}

fn num(p: &Json, k: &str) -> Option<f64> {
    p.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).filter(|f: &f64| f.is_finite())
}

fn text<'a>(p: &'a Json, k: &str) -> &'a str {
    p.get(k).and_then(Json::as_str).unwrap_or("")
}

/// The tool: a polygon layer of geodesic circles, with Tissot's parameters for the map's
/// projection when the map is flat.
pub fn run(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let spacing = num(p, "spacing").unwrap_or(30.0);
    if !(0.01..=90.0).contains(&spacing) {
        return Err("Spacing must be between 0.01° and 90°".into());
    }
    let radius_km = num(p, "radius").unwrap_or(500.0);
    if !(0.001..=2500.0).contains(&radius_km) {
        return Err("Radius must be between 1 m and 2500 km".into());
    }
    let metres = radius_km * 1000.0;
    let reach = metres / (EARTH_RADIUS_M * std::f64::consts::PI / 180.0);
    let in_view = text(p, "area") == "Map Extent";
    let area = if in_view {
        inp.view_extent().ok_or("There's no map view to fill; choose World")?
    } else {
        Extent::new(-180.0, -90.0, 180.0, 90.0)
    };
    let points = centres(area, spacing, reach, !in_view, MAX_CIRCLES)
        .map_err(|n| format!("{n} circles is too many (at most {MAX_CIRCLES}): use a larger spacing"))?;
    if points.is_empty() {
        return Err(format!("No grid point every {spacing}° falls in the area: use a smaller spacing"));
    }
    let projection = inp.projection_name();
    let mut fields = vec![Field::new("lat", FieldType::Double), Field::new("lon", FieldType::Double)];
    if projection.is_some() {
        fields.push(Field::new("projection", FieldType::Text));
        for f in ["h", "k", "a", "b", "area_scale", "max_angle"] {
            fields.push(Field::new(f, FieldType::Double));
        }
    }
    let name = match text(p, "output").trim() {
        "" => "Tissot".to_string(),
        n => n.to_string(),
    };
    let mut out = Dataset::new(name, GeometryType::Polygon, fields);
    let round = |v: f64| (v * 1e4).round() / 1e4;
    let project = |c: Coord| inp.project(c);
    let mut unmeasured = 0;
    for c in &points {
        let mut attrs = vec![Value::Float(c.y), Value::Float(c.x)];
        if let Some(label) = &projection {
            attrs.push(Value::Text(label.clone()));
            match distortion(&project, *c) {
                Some(d) => attrs.extend([d.h, d.k, d.a, d.b, d.area, d.max_angle].map(|v| Value::Float(round(v)))),
                None => {
                    unmeasured += 1;
                    attrs.extend(std::iter::repeat_n(Value::Null, 6));
                }
            }
        }
        out.push(Some(Geometry::Polygon(vec![vec![circle(*c, metres)]])), attrs);
    }
    let n = out.features.len();
    let mut msg = format!("Tissot's indicatrix: {n} circles of {radius_km} km, every {spacing}°");
    msg.push_str(&match &projection {
        Some(label) => format!("; distortion fields for {label} (they don't follow later projection changes)"),
        None => "; no distortion fields on the globe".to_string(),
    });
    if unmeasured > 0 {
        msg.push_str(&format!("; {unmeasured} at the projection's edge or beyond it have none"));
    }
    Ok(ToolResult { losses: Vec::new(), output: Output::Layer(out), messages: vec![msg] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::{Distance, Geodesic, Point};
    use rustgis_geom::lonlat_to_merc;

    /// No map: no projection, no view.
    struct NoMap;
    impl Inputs for NoMap {
        fn dataset(&self, _: &str) -> Option<&Dataset> {
            None
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
    }

    /// A flat Web Mercator map showing an extent.
    struct Mercator(Extent);
    impl Inputs for Mercator {
        fn dataset(&self, _: &str) -> Option<&Dataset> {
            None
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
        fn projection_name(&self) -> Option<String> {
            Some("Web Mercator".into())
        }
        fn project(&self, c: Coord) -> Option<Coord> {
            Some(lonlat_to_merc(c))
        }
        fn view_extent(&self) -> Option<Extent> {
            Some(self.0)
        }
    }

    fn layer(r: ToolResult) -> Dataset {
        match r.output {
            Output::Layer(d) => d,
            _ => panic!("expected a layer"),
        }
    }

    fn float(ds: &Dataset, row: usize, field: &str) -> f64 {
        let i = ds.fields.iter().position(|f| f.name == field).unwrap();
        match ds.value(row, i) {
            Value::Float(v) => *v,
            v => panic!("{field}: {v:?}"),
        }
    }

    #[test]
    fn the_classic_world_grid_of_geodesic_circles() {
        let ds = layer(run(&NoMap, &serde_json::json!({})).unwrap());
        // Every 30° from 60°S to 60°N, and from 150°W to 150°E (none on the antimeridian).
        assert_eq!(ds.features.len(), 5 * 11);
        assert_eq!(ds.name, "Tissot");
        // Without a flat map, just the centres.
        assert_eq!(ds.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["lat", "lon"]);
        for (row, f) in ds.features.iter().enumerate() {
            let centre = Coord::new(float(&ds, row, "lon"), float(&ds, row, "lat"));
            let Some(Geometry::Polygon(polys)) = &f.geometry else { panic!() };
            let ring = &polys[0][0];
            assert_eq!(ring.len(), VERTICES + 1);
            assert_eq!(ring.first(), ring.last());
            for v in ring {
                // 500 km on the WGS 84 ellipsoid, as GeographicLib measures it through `geo`.
                let d = Geodesic.distance(Point::new(centre.x, centre.y), Point::new(v.x, v.y));
                assert!((d - 500_000.0).abs() < 1e-3, "{centre:?} → {v:?}");
            }
        }
    }

    #[test]
    fn web_mercator_keeps_shapes_and_grows_areas() {
        let ds = layer(run(&Mercator(Extent::new(-180.0, -90.0, 180.0, 90.0)), &serde_json::json!({})).unwrap());
        let row =
            |lat: f64, lon: f64| (0..ds.features.len()).find(|&r| float(&ds, r, "lat") == lat && float(&ds, r, "lon") == lon).unwrap();
        let (eq, n60) = (row(0.0, 0.0), row(60.0, 30.0));
        // Near 1 at the Equator (Web Mercator's sphere is the ellipsoid's equator).
        assert!((float(&ds, eq, "k") - 1.0).abs() < 1e-3 && (float(&ds, eq, "area_scale") - 1.0).abs() < 0.01);
        // At 60°: scale about sec 60° = 2 both ways, so area about 4 and shapes kept.
        assert!((float(&ds, n60, "h") - 2.0).abs() < 0.01 && (float(&ds, n60, "k") - 2.0).abs() < 0.01);
        assert!((float(&ds, n60, "area_scale") - 4.0).abs() < 0.03);
        // Web Mercator isn't quite conformal on the ellipsoid, but nearly.
        assert!(float(&ds, n60, "max_angle") < 0.5);
        assert!(ds.features.iter().enumerate().all(|(r, _)| float(&ds, r, "a") >= float(&ds, r, "b")));
    }

    #[test]
    fn an_equal_area_projection_keeps_area_and_bends_angles() {
        // Lambert's cylindrical equal-area projection on the sphere, through the authalic radius:
        // area scale 1 everywhere (to the sphere-versus-ellipsoid difference), angles bent away
        // from the Equator.
        let r = 6_371_007.2;
        let project = |c: Coord| Some(Coord::new(r * c.x.to_radians(), r * c.y.to_radians().sin()));
        let at_60 = distortion(&project, Coord::new(0.0, 60.0)).unwrap();
        assert!((at_60.area - 1.0).abs() < 0.01, "{at_60:?}");
        assert!((at_60.k - 2.0).abs() < 0.02 && (at_60.h - 0.5).abs() < 0.01, "{at_60:?}");
        assert!(at_60.max_angle > 60.0, "{at_60:?}");
    }

    /// A centre on the antimeridian, or beyond ±180° (as a Map Extent grid can give), is measured
    /// where it is on the Earth; one whose sides fall on opposite edges gets no parameters.
    #[test]
    fn centres_on_and_past_the_antimeridian() {
        // Web Mercator with x wrapped to ±π·R, as a world projection's plane is.
        let wrapped = |c: Coord| {
            let m = lonlat_to_merc(c);
            let half = std::f64::consts::PI * 6_378_137.0;
            Some(Coord::new((m.x + half).rem_euclid(2.0 * half) - half, m.y))
        };
        let plain = |c: Coord| Some(lonlat_to_merc(c));
        let at_60 = distortion(&plain, Coord::new(10.0, 60.0)).unwrap();
        // Past ±180°: the same as the place it stands for.
        assert_eq!(distortion(&plain, Coord::new(370.0, 60.0)), Some(at_60));
        // On the edge, east and west land on opposite sides of the plane: refused, not huge.
        assert_eq!(distortion(&wrapped, Coord::new(180.0, 0.0)), None);
        // A degree away, both sides are on the same edge: measured as on the unwrapped plane.
        let (w, p) = (distortion(&wrapped, Coord::new(179.0, 0.0)).unwrap(), distortion(&plain, Coord::new(179.0, 0.0)).unwrap());
        assert!([(w.h, p.h), (w.k, p.k), (w.area, p.area)].iter().all(|(a, b)| (a - b).abs() < 1e-6), "{w:?} {p:?}");
    }

    #[test]
    fn the_map_extent_and_the_limits() {
        // Around Espírito Santo, every 0.5° with 10 km circles.
        let es = Mercator(Extent::new(-42.0, -21.5, -39.5, -17.8));
        let ds = layer(run(&es, &serde_json::json!({"area": "Map Extent", "spacing": 0.5, "radius": 10, "output": "ES"})).unwrap());
        assert_eq!(ds.name, "ES");
        assert_eq!(ds.features.len(), 6 * 8, "longitudes -42..-39.5, latitudes -21.5..-18");
        // No view to fill, a grid that misses the area, or too many circles.
        assert!(run(&NoMap, &serde_json::json!({"area": "Map Extent"})).is_err());
        assert!(run(&es, &serde_json::json!({"area": "Map Extent"})).is_err_and(|e| e.contains("smaller spacing")));
        assert!(run(&NoMap, &serde_json::json!({"spacing": 1, "radius": 10})).is_err_and(|e| e.contains("too many")));
        // The finest spacing over the world is refused by its count, before a point is made
        // (about 650 million of them would be gigabytes).
        assert!(run(&NoMap, &serde_json::json!({"spacing": 0.01, "radius": 0.001})).is_err_and(|e| e.contains("too many")));
        // Grid values read as typed, without binary noise.
        let tenth = layer(run(&es, &serde_json::json!({"area": "Map Extent", "spacing": 0.1, "radius": 1})).unwrap());
        assert!((0..tenth.features.len()).any(|r| float(&tenth, r, "lat") == -21.3));
        // A view more than a turn wide holds each place once.
        let wide = Mercator(Extent::new(170.0, -10.0, 560.0, 10.0));
        let ds = layer(run(&wide, &serde_json::json!({"area": "Map Extent", "spacing": 30, "radius": 100})).unwrap());
        let mut places: Vec<i64> = (0..ds.features.len()).map(|r| (float(&ds, r, "lon").rem_euclid(360.0) * 10.0).round() as i64).collect();
        let n = places.len();
        places.sort_unstable();
        places.dedup();
        assert_eq!(places.len(), n, "each longitude once");
        assert_eq!(n, 12, "a turn of 30° steps at the equator");
        assert!((0..n).all(|r| float(&ds, r, "lon") >= 170.0), "the view's own longitudes");
        // A spacing that doesn't divide a turn keeps every column until one repeats: at 70° over
        // 0..430°, all seven (0 … 420), none a turn from another.
        let seventy = Mercator(Extent::new(0.0, -10.0, 430.0, 10.0));
        let ds = layer(run(&seventy, &serde_json::json!({"area": "Map Extent", "spacing": 70, "radius": 100})).unwrap());
        assert_eq!(ds.features.len(), 7);
        // A view far past the Earth is held to it: no endless grid, no overflow.
        let huge = Mercator(Extent::new(-1e300, -1e300, 1e300, 1e300));
        assert!(run(&huge, &serde_json::json!({"area": "Map Extent", "spacing": 0.01})).is_err_and(|e| e.contains("too many")));
        let ds = layer(run(&huge, &serde_json::json!({"area": "Map Extent", "spacing": 30, "radius": 100})).unwrap());
        assert!((0..ds.features.len()).all(|r| float(&ds, r, "lat").abs() <= 90.0 && float(&ds, r, "lon").abs() <= 540.0));
        assert!(run(&NoMap, &serde_json::json!({"radius": 0})).is_err());
        // Circles that would reach a pole are left out.
        let ds = layer(run(&NoMap, &serde_json::json!({"spacing": 10, "radius": 2000})).unwrap());
        assert!((0..ds.features.len()).all(|r| float(&ds, r, "lat").abs() <= 70.0));
    }
}
