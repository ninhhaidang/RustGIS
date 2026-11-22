//! Celestial bodies: the Earth, the Moon, Mars and the other bodies a map can show (#223).
//!
//! Coordinates stay longitude/latitude degrees and the map stays a Web Mercator sphere whatever
//! the body; what changes is how big that sphere is on the ground. Every measurement here is made
//! on the Earth's sphere and comes from angles, so a body's lengths are the Earth's times the ratio
//! of their mean radii ([`Body::ratio`]) and its areas the Earth's times its square. That is exact
//! for a sphere, which is how every body but the Earth and Mars is modelled, and within Mars'
//! flattening (0.6%) for Mars.

/// A celestial body: a rotational ellipsoid by its equatorial radius and flattening.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body {
    /// The id a map stores (`"earth"`, `"mars"`…).
    pub id: &'static str,
    /// Its name, in English.
    pub name: &'static str,
    /// Equatorial (semi-major) radius, metres.
    pub a: f64,
    /// Inverse flattening; 0 for a sphere.
    pub inv_f: f64,
}

/// The Earth (WGS 84), the body a map is on unless told otherwise.
pub const EARTH: Body = Body { id: "earth", name: "Earth", a: 6_378_137.0, inv_f: 298.257_223_563 };

/// The bodies a map can be on: the Earth (WGS 84) and the IAU figures of the others, the Moon and
/// Mars first, then the rest by name.
pub const BODIES: [Body; 12] = [
    EARTH,
    Body { id: "moon", name: "Moon", a: 1_737_400.0, inv_f: 0.0 },
    Body { id: "mars", name: "Mars", a: 3_396_190.0, inv_f: 169.894_447 },
    Body { id: "callisto", name: "Callisto", a: 2_410_300.0, inv_f: 0.0 },
    Body { id: "charon", name: "Charon", a: 606_000.0, inv_f: 0.0 },
    Body { id: "europa", name: "Europa", a: 1_560_800.0, inv_f: 0.0 },
    Body { id: "ganymede", name: "Ganymede", a: 2_631_200.0, inv_f: 0.0 },
    Body { id: "io", name: "Io", a: 1_821_600.0, inv_f: 0.0 },
    Body { id: "mercury", name: "Mercury", a: 2_439_400.0, inv_f: 0.0 },
    Body { id: "pluto", name: "Pluto", a: 1_188_300.0, inv_f: 0.0 },
    Body { id: "titan", name: "Titan", a: 2_574_730.0, inv_f: 0.0 },
    Body { id: "venus", name: "Venus", a: 6_051_800.0, inv_f: 0.0 },
];

impl Body {
    /// The body with this id or name (any case), if there is one.
    pub fn find(s: &str) -> Option<&'static Body> {
        let s = s.trim();
        BODIES.iter().find(|b| b.id.eq_ignore_ascii_case(s) || b.name.eq_ignore_ascii_case(s))
    }

    /// The body with this id, or the Earth for an unknown one (as a project file may hold).
    pub fn get(id: &str) -> &'static Body {
        Body::find(id).unwrap_or(&EARTH)
    }

    pub fn is_earth(&self) -> bool {
        self.id == EARTH.id
    }

    /// Polar radius, metres.
    pub fn b(&self) -> f64 {
        if self.inv_f > 0.0 { self.a * (1.0 - 1.0 / self.inv_f) } else { self.a }
    }

    /// Mean radius `(2a + b) / 3`, metres.
    pub fn mean_radius_m(&self) -> f64 {
        (2.0 * self.a + self.b()) / 3.0
    }

    /// Ground lengths on this body over the same lengths measured on the Earth's sphere
    /// ([`crate::EARTH_RADIUS_M`]): exactly 1 on the Earth.
    pub fn ratio(&self) -> f64 {
        if self.is_earth() { 1.0 } else { self.mean_radius_m() / crate::EARTH_RADIUS_M }
    }

    /// A length measured on the Earth's sphere ([`crate::geodesic_length_m`]) as this body's.
    pub fn length(&self, earth_m: f64) -> f64 {
        earth_m * self.ratio()
    }

    /// An area measured on the Earth's sphere ([`crate::geodesic_ring_area_m2`]) as this body's.
    pub fn area(&self, earth_m2: f64) -> f64 {
        earth_m2 * self.ratio() * self.ratio()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(clippy::disallowed_methods, reason = "another body measures on its sphere")]
    fn bodies_scale_measurements_by_their_radius() {
        assert_eq!(Body::get("earth").ratio(), 1.0);
        assert_eq!(Body::get("nope").id, "earth");
        assert_eq!(Body::find("MARS").map(|b| b.id), Some("mars"));
        assert_eq!(Body::find("Moon").map(|b| b.id), Some("moon"));
        // A quarter of the Moon's equator.
        let moon = Body::get("moon");
        let quarter = moon.length(crate::haversine_m(crate::Coord::new(0.0, 0.0), crate::Coord::new(90.0, 0.0)));
        assert!((quarter - std::f64::consts::FRAC_PI_2 * 1_737_400.0).abs() < 1e-3, "{quarter}");
        // Mars' mean radius, (2a + b) / 3, and its areas the square of the ratio.
        let mars = Body::get("mars");
        assert!((mars.mean_radius_m() - 3_389_526.7).abs() < 0.1, "{}", mars.mean_radius_m());
        assert!((mars.area(1.0) - mars.ratio().powi(2)).abs() < 1e-12);
        // Every id is distinct and every body is real.
        for (i, b) in BODIES.iter().enumerate() {
            assert!(b.a > 0.0 && b.ratio() > 0.0 && b.ratio().is_finite());
            assert!(BODIES.iter().skip(i + 1).all(|o| o.id != b.id));
        }
    }
}
