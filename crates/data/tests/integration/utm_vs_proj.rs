//! The UTM series in `rustgis-geom` (Karney's Krüger series) against terrio-crs, which reprojects
//! rasters and layers: the same places in every zone and band, both hemispheres, the Norway
//! and Svalbard zones and the zones' edges, within a millimeter. And UPS, the grid of the polar
//! areas, against terrio-crs's polar stereographic at UPS's scale of 0.994 and 2,000,000 m false
//! origin (EPSG:5041 north, 5042 south), at random polar places and at the poles.

use rustgis_data::raster::Crs;
use rustgis_geom::{Coord, ups, utm};

/// A small deterministic generator (no dependency).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn proj(zone: u8, north: bool) -> rustgis_data::raster::Transformer {
    let south = if north { "" } else { " +south" };
    Crs::parse(&format!("+proj=utm +zone={zone}{south} +datum=WGS84 +units=m +no_defs")).unwrap().transformer().unwrap()
}

/// Compares one place both ways; returns the largest difference in metres and degrees.
fn compare(t: &rustgis_data::raster::Transformer, p: Coord, zone: u8, north: bool) -> (f64, f64) {
    let (e, n) = utm::to_utm_in_zone(p, zone, north).unwrap();
    let want = t.from_lonlat(p).unwrap();
    let dm = (e - want.x).abs().max((n - want.y).abs());
    assert!(dm < 1e-3, "zone {zone} north {north} at {p:?}: ours {e} {n}, terrio-crs {} {}", want.x, want.y);
    let back = utm::from_utm(zone, north, want.x, want.y).unwrap();
    let theirs = t.to_lonlat(want).unwrap();
    let dd = (back.x - theirs.x).abs().max((back.y - theirs.y).abs());
    // A millimetre is about 9e-9° of latitude.
    assert!(dd < 1e-8, "zone {zone} north {north} at {p:?}: ours {back:?}, terrio-crs {theirs:?}");
    (dm, dd)
}

#[test]
fn random_places_in_every_zone_agree_with_terrio_crs() {
    let mut r = Lcg(2026);
    let (mut worst_m, mut worst_deg) = (0.0f64, 0.0f64);
    let mut count = 0;
    for zone in 1u8..=60 {
        for north in [true, false] {
            let t = proj(zone, north);
            for _ in 0..40 {
                let lat = if north { r.next() * utm::UTM_MAX_LAT } else { -r.next() * -utm::UTM_MIN_LAT };
                // Across the zone and a little past its edges, as Norway's and Svalbard's do.
                let lon = utm::central_meridian(zone) + (r.next() - 0.5) * 9.0;
                let p = Coord::new(lon, lat);
                let (m, d) = compare(&t, p, zone, north);
                worst_m = worst_m.max(m);
                worst_deg = worst_deg.max(d);
                count += 1;
            }
        }
    }
    eprintln!("{count} places: largest difference {worst_m:.2e} m, {worst_deg:.2e}°");
}

#[test]
fn standard_zones_bands_and_exceptions_agree_with_terrio_crs() {
    let places = [
        // GeographicLib's GeoConvert example (Baghdad): 38n 444140.54 3684706.36.
        (44.4, 33.3),
        // Norway's 32V west of 6° E, and Bergen.
        (3.5, 56.5),
        (5.32, 60.39),
        (11.99, 63.9),
        // Svalbard's wide zones: their edges, 6° from the central meridian.
        (0.01, 79.0),
        (8.99, 72.1),
        (9.01, 83.9),
        (20.99, 75.0),
        (21.01, 80.0),
        (32.99, 77.0),
        (33.01, 78.0),
        (41.99, 83.99),
        // The antimeridian, the equator, the band edges and the UTM limits.
        (-180.0, 0.0),
        (179.999, -0.001),
        (-174.0, 8.0),
        (0.0, -80.0),
        (-0.001, 83.999),
        (151.21, -33.87),
        (-74.006, 40.7128),
    ];
    for (lon, lat) in places {
        let p = Coord::new(lon, lat);
        let u = utm::to_utm(p).unwrap();
        let t = proj(u.zone, u.north());
        compare(&t, p, u.zone, u.north());
        let want = t.from_lonlat(p).unwrap();
        assert!((u.easting - want.x).abs() < 1e-3 && (u.northing - want.y).abs() < 1e-3, "{p:?} {u:?} {want:?}");
        // The EPSG codes the app reprojects with name the same systems.
        let epsg = if u.north() { 32600 } else { 32700 } + u32::from(u.zone);
        let by_code = Crs::from_epsg(epsg).unwrap().transformer().unwrap().from_lonlat(p).unwrap();
        assert!((by_code.x - want.x).abs() < 1e-6 && (by_code.y - want.y).abs() < 1e-6, "EPSG:{epsg}");
    }
}

fn ups_proj(north: bool) -> rustgis_data::raster::Transformer {
    let lat = if north { 90 } else { -90 };
    Crs::parse(&format!("+proj=stere +lat_0={lat} +lat_ts={lat} +k=0.994 +x_0=2000000 +y_0=2000000 +datum=WGS84 +units=m +no_defs"))
        .unwrap()
        .transformer()
        .unwrap()
}

/// Compares one polar place both ways; returns the largest difference in meters.
fn compare_ups(t: &rustgis_data::raster::Transformer, p: Coord, north: bool) -> f64 {
    let (e, n) = ups::to_ups_in_zone(p, north).unwrap();
    let want = t.from_lonlat(p).unwrap();
    let dm = (e - want.x).abs().max((n - want.y).abs());
    assert!(dm < 1e-3, "north {north} at {p:?}: ours {e} {n}, terrio-crs {} {}", want.x, want.y);
    let back = ups::from_ups(north, want.x, want.y).unwrap();
    let theirs = t.to_lonlat(want).unwrap();
    // Compare on the ground: near the pole a millimeter is many degrees of longitude.
    let (x, y) = ups::to_ups_in_zone(back, north).unwrap();
    let (tx, ty) = ups::to_ups_in_zone(theirs, north).unwrap();
    assert!((x - tx).abs().max((y - ty).abs()) < 1e-3, "north {north} at {p:?}: ours {back:?}, terrio-crs {theirs:?}");
    dm
}

#[test]
fn random_polar_places_agree_with_terrio_crs() {
    let mut r = Lcg(2026);
    let (north_t, south_t) = (ups_proj(true), ups_proj(false));
    let mut worst = 0.0f64;
    for _ in 0..5_000 {
        let lon = -180.0 + r.next() * 360.0;
        worst = worst.max(compare_ups(&north_t, Coord::new(lon, 84.0 + r.next() * 6.0), true));
        worst = worst.max(compare_ups(&south_t, Coord::new(lon, -80.0 - 1e-9 - r.next() * (10.0 - 2e-9)), false));
    }
    for (p, north) in [(Coord::new(0.0, 90.0), true), (Coord::new(0.0, -90.0), false), (Coord::new(180.0, 84.0), true)] {
        worst = worst.max(compare_ups(if north { &north_t } else { &south_t }, p, north));
    }
    assert!(worst < 1e-3, "worst {worst} m");
}
