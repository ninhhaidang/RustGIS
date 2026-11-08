//! The equidistant cylindrical projection on an ellipsoid (EPSG method 1028, PROJ's `eqc`): the
//! system of EPSG:4087 (WGS 84 / World Equidistant Cylindrical) and of the ellipsoidal
//! equirectangular systems of other bodies. proj4rs computes `eqc` on a sphere of the semi-major
//! axis only, which puts a point at 45°N about 24 km off on WGS 84, so the ellipsoidal form is
//! computed here.
//!
//! From the EPSG Guidance Note 7-2 (IOGP Publication 373-7-2), method 1028:
//! E = FE + ν₁·cos φ₁·(λ − λ₀) and N = FN + M, where φ₁ is the latitude of true scale,
//! ν₁ = a / √(1 − e²·sin²φ₁) the radius of curvature in the prime vertical there (Snyder 1987,
//! *Map Projections — A Working Manual*, USGS Professional Paper 1395, eq. 4-20) and M the
//! meridian distance from the equator. PROJ's `+lat_0` moves the origin north, to M − M(φ₀).
//!
//! The meridian distance is Snyder's series (eq. 3-21, chapter 3) carried to e⁸, as GN7-2 gives
//! it: the terms left out come to under a tenth of a millimetre on the Earth and on Mars. Its
//! inverse starts from Snyder's footpoint latitude (the rectifying latitude μ of eq. 7-19 and the
//! series of eqs. 3-24 and 3-26) and is finished by Newton's method with dM/dφ = ρ, the radius of
//! curvature in the meridian (eq. 4-18), so that both directions agree to well under a millimetre.
//! The series holds for ellipsoids up to [`MAX_ES`]; flatter ones (the giant planets') are refused.

use crate::Coord;

/// The largest eccentricity squared computed: e² = 0.02, a flattening of about 1/100. Up to it the
/// terms the meridian-distance series leaves out stay under 2·10⁻¹⁰ of the semi-major axis (1.2 mm
/// on a body the Earth's size; Mars, at e² = 0.0116, is under 10⁻¹¹). They grow as e¹⁰: 3.4 m on
/// the Earth's axis at a flattening of 1/20, 100 m at 1/10 (Saturn's), so flatter ellipsoids are
/// refused rather than approximated.
pub const MAX_ES: f64 = 0.02;

/// An equidistant cylindrical projection on an ellipsoid of semi-major axis `a` and eccentricity
/// squared `es` (0 for a sphere), with its latitude of true scale, origin and false origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EquidistantCylindrical {
    /// Semi-major axis (metres).
    pub a: f64,
    /// Eccentricity squared: 0 for a sphere.
    pub es: f64,
    /// Latitude of true scale, φ₁ (degrees).
    pub lat_ts: f64,
    /// Latitude of the origin, φ₀ (degrees).
    pub lat0: f64,
    /// Central meridian, λ₀ (degrees).
    pub lon0: f64,
    /// False easting and northing (metres).
    pub x0: f64,
    pub y0: f64,
    /// ν₁·cos φ₁: metres of easting per radian of longitude.
    across: f64,
    /// The meridian distance of the origin's latitude, M(φ₀) (metres).
    m0: f64,
    /// The meridian distance of the pole (metres): northings past it are off the map.
    pole: f64,
}

impl EquidistantCylindrical {
    /// A projection on an ellipsoid of semi-major axis `a` (metres) and eccentricity squared `es`
    /// (0 for a sphere). `None` for an impossible ellipsoid or one flatter than [`MAX_ES`], a
    /// parameter that isn't finite, a latitude of true scale at a pole (no scale along it) or an
    /// origin past one.
    pub fn new(a: f64, es: f64, lat_ts: f64, lat0: f64, lon0: f64, x0: f64, y0: f64) -> Option<EquidistantCylindrical> {
        let finite = [a, es, lat_ts, lat0, lon0, x0, y0].iter().all(|v| v.is_finite());
        if !finite || a <= 0.0 || !(0.0..=MAX_ES).contains(&es) || lat_ts.abs() >= 90.0 || lat0.abs() > 90.0 {
            return None;
        }
        let s1 = lat_ts.to_radians().sin();
        let across = a * lat_ts.to_radians().cos() / (1.0 - es * s1 * s1).sqrt();
        let m0 = meridian_distance(a, es, lat0.to_radians());
        let pole = meridian_distance(a, es, std::f64::consts::FRAC_PI_2);
        let p = EquidistantCylindrical { a, es, lat_ts, lat0, lon0, x0, y0, across, m0, pole };
        [across, m0, pole].iter().all(|v| v.is_finite() && v.abs() < 1e300).then_some(p)
    }

    /// Longitude/latitude (degrees, on its own ellipsoid and prime meridian) to projected metres.
    /// Longitudes are taken from the central meridian the short way round, ±180° on their own side.
    pub fn forward(&self, c: Coord) -> Option<Coord> {
        if !c.is_finite() || c.y.abs() > 90.0 {
            return None;
        }
        let d = c.x - self.lon0;
        let d = if d.abs() <= 180.0 + 1e-9 { d.clamp(-180.0, 180.0) } else { (d + 540.0).rem_euclid(360.0) - 180.0 };
        let x = self.x0 + self.across * d.to_radians();
        let y = self.y0 + meridian_distance(self.a, self.es, c.y.to_radians()) - self.m0;
        let out = Coord::new(x, y);
        out.is_finite().then_some(out)
    }

    /// Projected metres to longitude/latitude (degrees). Longitudes come back within ±180°, as
    /// PROJ gives them; `None` for a northing past a pole.
    pub fn inverse(&self, m: Coord) -> Option<Coord> {
        if !m.is_finite() {
            return None;
        }
        let distance = m.y - self.y0 + self.m0;
        // A millimetre past the pole is the pole; more is off the map.
        if distance.abs() > self.pole + 1e-3 {
            return None;
        }
        let half = std::f64::consts::FRAC_PI_2;
        let phi = latitude_of(self.a, self.es, distance.clamp(-self.pole, self.pole))?.clamp(-half, half);
        let lon = self.lon0 + ((m.x - self.x0) / self.across).to_degrees();
        if !lon.is_finite() {
            return None;
        }
        let lon = if (-180.0..=180.0).contains(&lon) { lon } else { (lon + 540.0).rem_euclid(360.0) - 180.0 };
        Some(Coord::new(lon, phi.to_degrees()))
    }
}

/// The meridian distance from the equator to latitude `phi` (radians) on an ellipsoid of
/// semi-major axis `a` and eccentricity squared `es`: Snyder 1987, eq. 3-21, with the e⁸ terms of
/// the same series as the EPSG Guidance Note 7-2 gives it for method 1028.
fn meridian_distance(a: f64, es: f64, phi: f64) -> f64 {
    let (e2, e4, e6, e8) = (es, es * es, es * es * es, es * es * es * es);
    a * ((1.0 - e2 / 4.0 - 3.0 * e4 / 64.0 - 5.0 * e6 / 256.0 - 175.0 * e8 / 16384.0) * phi
        - (3.0 * e2 / 8.0 + 3.0 * e4 / 32.0 + 45.0 * e6 / 1024.0 + 105.0 * e8 / 4096.0) * (2.0 * phi).sin()
        + (15.0 * e4 / 256.0 + 45.0 * e6 / 1024.0 + 525.0 * e8 / 16384.0) * (4.0 * phi).sin()
        - (35.0 * e6 / 3072.0 + 175.0 * e8 / 12288.0) * (6.0 * phi).sin()
        + (315.0 * e8 / 131_072.0) * (8.0 * phi).sin())
}

/// The latitude (radians) whose meridian distance is `distance` metres: Snyder's footpoint
/// latitude (eqs. 7-19, 3-24 and 3-26), refined by Newton's method on eq. 3-21 with its
/// derivative, the meridian's radius of curvature ρ = a(1 − e²) / (1 − e²·sin²φ)^{3/2} (eq. 4-18).
/// `None` when it doesn't settle within ten steps.
fn latitude_of(a: f64, es: f64, distance: f64) -> Option<f64> {
    let (e2, e4, e6) = (es, es * es, es * es * es);
    // (A first guess: Newton's method below settles it on eq. 3-21 as computed here.)
    let mu = distance / (a * (1.0 - e2 / 4.0 - 3.0 * e4 / 64.0 - 5.0 * e6 / 256.0));
    let r = (1.0 - es).sqrt();
    let e1 = (1.0 - r) / (1.0 + r);
    let (e1_2, e1_3, e1_4) = (e1 * e1, e1 * e1 * e1, e1 * e1 * e1 * e1);
    let mut phi = mu
        + (3.0 * e1 / 2.0 - 27.0 * e1_3 / 32.0) * (2.0 * mu).sin()
        + (21.0 * e1_2 / 16.0 - 55.0 * e1_4 / 32.0) * (4.0 * mu).sin()
        + (151.0 * e1_3 / 96.0) * (6.0 * mu).sin()
        + (1097.0 * e1_4 / 512.0) * (8.0 * mu).sin();
    for _ in 0..10 {
        let s = phi.sin();
        let w = 1.0 - es * s * s;
        let rho = a * (1.0 - es) / (w * w.sqrt());
        let step = (meridian_distance(a, es, phi) - distance) / rho;
        phi -= step;
        if !phi.is_finite() {
            return None;
        }
        if step.abs() < 1e-14 {
            return Some(phi);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WGS 84's semi-major axis and eccentricity squared.
    const A: f64 = 6_378_137.0;
    fn es_of(rf: f64) -> f64 {
        let f = 1.0 / rf;
        f * (2.0 - f)
    }

    /// The meridian distance by numerical integration of ρ (Simpson's rule), independently of
    /// the series.
    fn integrated(a: f64, es: f64, phi: f64) -> f64 {
        let n = 20_000;
        let h = phi / n as f64;
        let rho = |t: f64| {
            let w = 1.0 - es * t.sin() * t.sin();
            a * (1.0 - es) / (w * w.sqrt())
        };
        let mut sum = rho(0.0) + rho(phi);
        for i in 1..n {
            sum += rho(i as f64 * h) * if i % 2 == 1 { 4.0 } else { 2.0 };
        }
        sum * h / 3.0
    }

    #[test]
    fn epsg_guidance_note_example() {
        // EPSG Guidance Note 7-2, method 1028, its example for EPSG:4087: 55°N, 10°E on WGS 84
        // is E = 1 113 194.91 m, N = 6 097 230.31 m.
        let p = EquidistantCylindrical::new(A, es_of(298.257_223_563), 0.0, 0.0, 0.0, 0.0, 0.0).unwrap();
        let m = p.forward(Coord::new(10.0, 55.0)).unwrap();
        assert!((m.x - 1_113_194.91).abs() < 0.005 && (m.y - 6_097_230.31).abs() < 0.005, "{m:?}");
        let back = p.inverse(m).unwrap();
        assert!((back.x - 10.0).abs() < 1e-11 && (back.y - 55.0).abs() < 1e-11, "{back:?}");
    }

    #[test]
    fn meridian_distance_matches_its_integral() {
        // On WGS 84, Clarke 1866 and a body as flattened as Mars (1/169.9), at every 5°.
        for (a, rf) in [(A, 298.257_223_563), (6_378_206.4, 294.978_698_2), (3_396_190.0, 169.894_447_223_612)] {
            let es = es_of(rf);
            for d in (0..=90).step_by(5) {
                let phi = f64::from(d).to_radians();
                let (series, exact) = (meridian_distance(a, es, phi), integrated(a, es, phi));
                assert!((series - exact).abs() < 1e-4, "{a} {d}°: {series} vs {exact}");
            }
        }
        // At the flattest ellipsoid computed, within 2·10⁻¹⁰ of the semi-major axis (MAX_ES).
        for d in (0..=90).step_by(5) {
            let phi = f64::from(d).to_radians();
            let (series, exact) = (meridian_distance(A, MAX_ES, phi), integrated(A, MAX_ES, phi));
            assert!((series - exact).abs() < 2e-10 * A, "{d}°: {series} vs {exact}");
        }
        // WGS 84's 45° parallel is 4 984 944.378 m from the equator along the meridian.
        assert!((meridian_distance(A, es_of(298.257_223_563), 45f64.to_radians()) - 4_984_944.378).abs() < 0.001);
    }

    #[test]
    fn round_trips_and_parameters() {
        let es = es_of(298.257_223_563);
        for (ts, lat0, lon0, x0, y0) in [
            (0.0, 0.0, 0.0, 0.0, 0.0),
            (30.0, 0.0, -90.0, 500_000.0, 0.0),
            (-60.0, 45.0, 150.0, 0.0, -1_000.0),
            (89.0, -90.0, 0.0, 0.0, 0.0),
        ] {
            let p = EquidistantCylindrical::new(A, es, ts, lat0, lon0, x0, y0).unwrap();
            // The origin is the false origin.
            let o = p.forward(Coord::new(lon0, lat0)).unwrap();
            assert!((o.x - x0).abs() < 1e-6 && (o.y - y0).abs() < 1e-6, "{o:?}");
            for (lon, lat) in [(0.0, 0.0), (10.0, 50.0), (-120.0, -35.0), (179.0, 89.0), (-60.0, 10.0), (149.0, -89.9), (33.0, 90.0)] {
                let m = p.forward(Coord::new(lon, lat)).unwrap();
                let back = p.inverse(m).unwrap();
                let dl = ((back.x - lon + 540.0).rem_euclid(360.0) - 180.0).abs();
                assert!(dl < 1e-9 && (back.y - lat).abs() < 1e-10, "{ts} {lat0} {lon0}: {lon},{lat} → {back:?}");
            }
            // Easting per degree is ν₁·cos φ₁ (eq. 4-20 for ν₁).
            let s = ts.to_radians().sin();
            let nu1 = A / (1.0 - es * s * s).sqrt();
            let step = p.forward(Coord::new(lon0 + 1.0, lat0)).unwrap().x - x0;
            assert!((step - nu1 * ts.to_radians().cos() * 1f64.to_radians()).abs() < 1e-6, "{step}");
        }
        // Past a pole, or not a number: nothing.
        let p = EquidistantCylindrical::new(A, es, 0.0, 0.0, 0.0, 0.0, 0.0).unwrap();
        assert!(p.inverse(Coord::new(0.0, 10_010_000.0)).is_none());
        assert!(p.inverse(Coord::new(0.0, -10_010_000.0)).is_none());
        assert!(p.inverse(Coord::new(f64::NAN, 0.0)).is_none());
        assert!(p.forward(Coord::new(0.0, 90.5)).is_none());
        assert!(p.forward(Coord::new(f64::INFINITY, 0.0)).is_none());
        // Longitudes past the edge come back within ±180°.
        let far = p.inverse(Coord::new(p.forward(Coord::new(170.0, 0.0)).unwrap().x + 2.0 * p.across, 0.0)).unwrap();
        assert!((far.x - (170.0 + 2f64.to_degrees() - 360.0)).abs() < 1e-9, "{far:?}");
    }

    /// A latitude Newton's method doesn't settle on within its steps is no latitude, not its
    /// last guess: on an ellipsoid as flat as e² = 0.96 it overshoots.
    #[test]
    fn unsettled_latitudes_are_none() {
        let es = 0.96;
        let pole = meridian_distance(A, es, std::f64::consts::FRAC_PI_2);
        assert!((1..100).any(|k| latitude_of(A, es, pole * f64::from(k) / 100.0).is_none()));
        // Where it settles, it is the latitude.
        for k in 0..=100 {
            let distance = meridian_distance(A, MAX_ES, std::f64::consts::FRAC_PI_2) * f64::from(k) / 100.0;
            let phi = latitude_of(A, MAX_ES, distance).unwrap();
            assert!((meridian_distance(A, MAX_ES, phi) - distance).abs() < 1e-6, "{k}");
        }
    }

    #[test]
    fn impossible_parameters_are_refused() {
        let es = es_of(298.257_223_563);
        for (a, es, ts, lat0) in [
            (0.0, es, 0.0, 0.0),
            (-1.0, es, 0.0, 0.0),
            (A, 1.0, 0.0, 0.0),
            (A, -0.1, 0.0, 0.0),
            (A, es, 90.0, 0.0),
            (A, es, -90.0, 0.0),
            (A, es, 0.0, 91.0),
            (f64::NAN, es, 0.0, 0.0),
            (A, es, f64::INFINITY, 0.0),
            (f64::MAX, es, 0.0, 0.0),
            // Flatter than the series holds for: Uranus (e² 0.045), Jupiter (0.126), Saturn (0.186).
            (25_559_000.0, 0.045, 0.0, 0.0),
            (71_492_000.0, 0.126, 0.0, 0.0),
            (A, MAX_ES + 1e-9, 0.0, 0.0),
        ] {
            assert!(EquidistantCylindrical::new(a, es, ts, lat0, 0.0, 0.0, 0.0).is_none(), "{a} {es} {ts} {lat0}");
        }
        assert!(EquidistantCylindrical::new(A, es, 0.0, 0.0, f64::NAN, 0.0, 0.0).is_none());
        // A huge but finite false origin projects nothing that isn't finite.
        let p = EquidistantCylindrical::new(A, MAX_ES, 0.0, 0.0, 0.0, 1e308, 1e308).unwrap();
        assert!(p.forward(Coord::new(180.0, 90.0)).is_none_or(|m| m.is_finite()));
        let _ = p.inverse(Coord::new(-1e308, -1e308));
    }
}
