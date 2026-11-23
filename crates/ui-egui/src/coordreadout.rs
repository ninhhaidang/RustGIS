//! How the pointer's coordinates read in the status bar and in the map's right-click menu: in the
//! map's own coordinate system, decimal degrees, degrees-minutes-seconds, UTM, MGRS or the active
//! layer's coordinate system. A click on
//! the status bar readout steps through them in that order (`ui.coordinateFormat`), and the choice
//! is remembered between sessions as the `coordinate_format` preference.

use rustgis_engine::geom::{self, Coord};
use rustgis_engine::map::projection::Flat;

use crate::i18n;

/// The preference that keeps the chosen format: its [`CoordReadout::code`].
pub const PREF: &str = "coordinate_format";

/// A coordinate readout format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoordReadout {
    /// `2600000 mE  1200000 mN`: the map's own coordinates, as its coordinate system counts them
    /// (on the globe and another body's map, decimal degrees; see [`MapSystem`]).
    #[default]
    Map,
    /// `2600000 mE  1200000 mN`: the active layer's coordinates, in the projected system it was
    /// read in (decimal degrees for a layer in degrees, or with no layer active; see
    /// [`MapSystem::of_layer`]).
    Layer,
    /// `12.3456° E  45.6789° N`
    Decimal,
    /// `12°20′44.2″ E  45°40′44.0″ N`
    Dms,
    /// `33north 412345 5523456`: zone and hemisphere, easting and northing in metres.
    Utm,
    /// `33U VP 12345 23456`: zone and band, the 100 km square, easting and northing to 1 m.
    Mgrs,
}

impl CoordReadout {
    /// Every format, in the order a click steps through them.
    pub const ALL: [CoordReadout; 6] =
        [CoordReadout::Map, CoordReadout::Decimal, CoordReadout::Dms, CoordReadout::Utm, CoordReadout::Mgrs, CoordReadout::Layer];

    /// The name commands and the preference use (never translated).
    pub fn code(self) -> &'static str {
        match self {
            CoordReadout::Map => "map",
            CoordReadout::Layer => "layer",
            CoordReadout::Decimal => "decimal",
            CoordReadout::Dms => "dms",
            CoordReadout::Utm => "utm",
            CoordReadout::Mgrs => "mgrs",
        }
    }

    /// The format a [`code`](Self::code) names, ignoring case and spaces around it.
    pub fn from_code(code: &str) -> Option<Self> {
        let code = code.trim();
        Self::ALL.into_iter().find(|f| f.code().eq_ignore_ascii_case(code))
    }

    /// The format after this one: map → decimal → DMS → UTM → MGRS → layer → map.
    pub fn next(self) -> Self {
        match self {
            CoordReadout::Map => CoordReadout::Decimal,
            CoordReadout::Layer => CoordReadout::Map,
            CoordReadout::Decimal => CoordReadout::Dms,
            CoordReadout::Dms => CoordReadout::Utm,
            CoordReadout::Utm => CoordReadout::Mgrs,
            CoordReadout::Mgrs => CoordReadout::Layer,
        }
    }

    /// The format's name, translated in logical order, for filling into [`i18n::fmt`] messages.
    pub fn label(self) -> &'static str {
        match self {
            CoordReadout::Map => tl_raw!("map coordinates"),
            CoordReadout::Layer => tl_raw!("layer coordinates"),
            CoordReadout::Decimal => tl_raw!("decimal degrees"),
            CoordReadout::Dms => tl_raw!("degrees, minutes and seconds"),
            CoordReadout::Utm => tl_raw!("UTM"),
            CoordReadout::Mgrs => tl_raw!("MGRS"),
        }
    }

    /// Whether this is a grid of the Earth (UTM, MGRS), which names no place on another body.
    pub fn earth_only(self) -> bool {
        matches!(self, CoordReadout::Utm | CoordReadout::Mgrs)
    }

    /// The system the map and layer formats read in: the map's own ([`MapSystem::of`]) or the
    /// active layer's ([`MapSystem::of_layer`]); `None` for the other formats.
    pub fn system(self, map: &rustgis_engine::map::Map, active: Option<u32>) -> Option<MapSystem> {
        match self {
            CoordReadout::Map => MapSystem::of(map),
            CoordReadout::Layer => MapSystem::of_layer(map, active),
            _ => None,
        }
    }

    /// The place in this format; for UTM and MGRS, UPS in the polar areas (north of 84° N and
    /// south of 80° S) and `None` on another body's map (`earth` false); in the map's or the
    /// layer's coordinates (`map`, [`CoordReadout::system`]) `None` where the system doesn't
    /// reach, and decimal degrees without one (`map` `None`); and for every format `None` for a
    /// non-finite place.
    pub fn format(self, c: Coord, earth: bool, map: Option<&MapSystem>) -> Option<String> {
        if !c.is_finite() || (self.earth_only() && !earth) {
            return None;
        }
        match self {
            CoordReadout::Map | CoordReadout::Layer => match map {
                Some(m) => m.format(c),
                None => Some(geom::format_lonlat(c)),
            },
            CoordReadout::Decimal => Some(geom::format_lonlat(c)),
            CoordReadout::Dms => Some(geom::format_dms(c)),
            CoordReadout::Utm => geom::format_utm(c),
            CoordReadout::Mgrs => geom::format_mgrs(c),
        }
    }

    /// The status bar's readout of the pointer: `—` off the map, and a short statement on another
    /// body's map, where the Earth's grids name no place, rather than a wrong grid reference (no
    /// wider than that reference). Map coordinates can run wider: up to 13 characters for each
    /// number, with three decimals far from the system's origin.
    pub fn readout(self, c: Option<Coord>, earth: bool, map: Option<&MapSystem>) -> String {
        match c {
            None => "—".to_string(),
            Some(c) if !c.is_finite() => "—".to_string(),
            Some(c) => self.format(c, earth, map).unwrap_or_else(|| i18n::fmt("No {format} here", &[("format", self.label())])),
        }
    }

    /// The status bar readout's hover text: the current format, what a click does, and how UTM
    /// and MGRS read near the poles.
    pub fn hover(self) -> String {
        i18n::fmt(
            "Coordinates in {format}. Click to switch: map coordinates, decimal degrees, DMS, UTM, MGRS, layer coordinates. Map coordinates are in the map's coordinate system, named beside them, and in degrees on the globe and on another body's map. Layer coordinates are in the projected coordinate system the active layer was read in, named beside them, and in degrees for a layer in degrees, with no layer active and on another body's map. UTM and MGRS are grids of the Earth, read in UPS north of 84° N and south of 80° S.",
            &[("format", self.label())],
        )
    }
}

/// The name [`MapSystem::name`] gives a projection.
fn system_name(p: &rustgis_engine::map::Projection) -> String {
    let label = p.label();
    if matches!(p, rustgis_engine::map::Projection::WebMercator) || label.starts_with("EPSG:") || label.starts_with("ESRI:") {
        label
    } else {
        p.code()
    }
}

/// The conversion of a layer's system, kept from the last frame that asked for the same system:
/// a system read from a file without a code (WKT) is otherwise built again each frame.
fn layer_conversion(crs: rustgis_engine::data::raster::Crs) -> Option<rustgis_engine::data::raster::Transformer> {
    static LAST: std::sync::Mutex<Option<(rustgis_engine::data::raster::Crs, rustgis_engine::data::raster::Transformer)>> =
        std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(t) = last.as_ref().filter(|(kept, _)| *kept == crs).map(|(_, t)| t.clone()) {
        return Some(t);
    }
    let t = crs.transformer().ok()?;
    *last = Some((crs, t.clone()));
    Some(t)
}

/// How a system gives a place's coordinates from its longitude and latitude.
enum Plane {
    /// A map's flat projection, as the map draws it (Web Mercator's too, for a layer in it).
    Flat(Flat),
    /// A layer's projected system, by its conversion, with the metres in one of its units.
    Data(rustgis_engine::data::raster::Transformer, f64),
}

/// A flat system of the Earth's own coordinates, the map's or a layer's, as the map and layer
/// formats read them: its projection and how many of its units a screen pixel spans, which sets
/// the decimals shown.
pub struct MapSystem {
    plane: Plane,
    res: f64,
    /// The system's name, never translated: see [`MapSystem::name`].
    name: String,
}

impl MapSystem {
    /// The system of a map: `None` on the globe and on another body's map, whose flat projection
    /// is computed on the Earth's figure and so gives no coordinates of its own body.
    pub fn of(map: &rustgis_engine::map::Map) -> Option<MapSystem> {
        (map.body().is_earth() && !map.projection.is_globe()).then(|| MapSystem {
            plane: Plane::Flat(Flat::of(&map.projection)),
            res: map.camera.res,
            name: system_name(&map.projection),
        })
    }

    /// The projected system the layer `active` was read in (a feature layer's, a raster's): `None`
    /// with no such layer, for one in degrees (longitude and latitude in any datum, which read as
    /// decimal degrees), on another body's map, for a system whose conversion can't be made or
    /// gives no unit, and for one whose axes aren't east, then north (a westing and southing,
    /// `+axis=wsu`, or a northing first), which an easting and northing would mislabel. A pixel
    /// spans the map's ground resolution, in the system's units. The conversion is kept for the
    /// next frame while the layer's system stays the same.
    pub fn of_layer(map: &rustgis_engine::map::Map, active: Option<u32>) -> Option<MapSystem> {
        use rustgis_engine::data::raster::Crs;
        if !map.body().is_earth() {
            return None;
        }
        let l = map.layer(active?)?;
        // A raster's system is its pixels', as Go To XY reads it.
        let crs = rustgis_engine::cmd::navigate::layer_coordinates_system(l)?;
        if crs.is_geographic() || crs.is_geocentric() || crs.is_planetary() {
            return None;
        }
        let ground = map.ground_res();
        let (plane, to_meter) = if crs == Crs::WebMercator {
            (Plane::Flat(Flat::Mercator), 1.0)
        } else {
            let t = layer_conversion(crs)?;
            if t.axis_directions() != [(1.0, 0.0), (0.0, 1.0)] {
                return None;
            }
            let to_meter = t.params().map(|p| p.to_meter).filter(|m| m.is_finite() && *m > 0.0)?;
            (Plane::Data(t, to_meter), to_meter)
        };
        let name = if crs == Crs::WebMercator { "Web Mercator".to_string() } else { crs.label() };
        Some(MapSystem { plane, res: ground / to_meter, name })
    }

    /// A system from its parts (tests).
    pub fn new(flat: Flat, res: f64, name: &str) -> MapSystem {
        MapSystem { plane: Plane::Flat(flat), res, name: name.to_string() }
    }

    /// The system's name, as it can be given back to `map.projection`: its EPSG or ESRI code
    /// (`EPSG:2056`), `Web Mercator`, else the map's projection code (`robinson +lon_0=150`,
    /// `local:7.4386,46.9511`), which, unlike a preset's or a local view's display name, names
    /// the system exactly.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The place's easting and northing, with as many decimals as a pixel needs (none at a metre
    /// a pixel or more, up to three) and the unit: `2600000 mE  1200000 mN`, `ftE` and `ftN` in
    /// international feet, `ftUSE` and `ftUSN` in US survey feet, and `E` and `N` alone in
    /// another unit. A place on a world copy past the antimeridian reads as the same place within
    /// ±180°. `None` where the projection doesn't reach (outside the area the map draws it over,
    /// for LV95 a 60° cap around Bern, well past Switzerland) or folds, as the map draws nothing
    /// there.
    fn format(&self, c: Coord) -> Option<String> {
        let c = Coord::new(crate::mapmenu::wrap_lon(c.x), c.y);
        let m = match &self.plane {
            Plane::Flat(f) => f.forward_checked(c),
            Plane::Data(t, _) => t.from_lonlat(c),
        }
        .filter(|m| m.is_finite())?;
        let decimals = if self.res.is_finite() && self.res > 0.0 { (-self.res.log10()).ceil().clamp(0.0, 3.0) as usize } else { 0 };
        let to_meter = match &self.plane {
            Plane::Flat(Flat::Mercator) => 1.0,
            Plane::Flat(Flat::Custom(p)) => p.to_meter(),
            Plane::Data(_, to_meter) => *to_meter,
        };
        let unit = if (to_meter - 1.0).abs() < 1e-9 {
            "m"
        } else if (to_meter - 0.3048).abs() < 1e-9 {
            "ft"
        } else if (to_meter - 1200.0 / 3937.0).abs() < 1e-9 {
            "ftUS"
        } else {
            ""
        };
        // Rounded first, then + 0.0: a coordinate that rounds to zero reads 0, not -0.
        let scale = 10f64.powi(decimals as i32);
        let shown = |v: f64| (v * scale).round() / scale + 0.0;
        Some(format!("{:.d$} {unit}E  {:.d$} {unit}N", shown(m.x), shown(m.y), d = decimals))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_steps_through_every_format_and_back() {
        let mut f = CoordReadout::default();
        assert_eq!(f, CoordReadout::Map);
        let mut seen = Vec::new();
        for _ in 0..CoordReadout::ALL.len() {
            seen.push(f);
            f = f.next();
        }
        assert_eq!(seen, CoordReadout::ALL);
        assert_eq!(f, CoordReadout::Map);
    }

    #[test]
    fn codes_round_trip() {
        for f in CoordReadout::ALL {
            assert_eq!(CoordReadout::from_code(f.code()), Some(f));
            assert_eq!(CoordReadout::from_code(&format!(" {} ", f.code().to_uppercase())), Some(f));
        }
        assert_eq!(CoordReadout::from_code("ups"), None);
        assert_eq!(CoordReadout::from_code(""), None);
    }

    #[test]
    fn readouts_in_each_format() {
        let p = Coord::new(44.4, 33.3);
        assert_eq!(CoordReadout::Decimal.readout(Some(p), true, None), "44.4000° E  33.3000° N");
        assert_eq!(CoordReadout::Dms.readout(Some(p), true, None), "44°24′00.0″ E  33°18′00.0″ N");
        assert_eq!(CoordReadout::Utm.readout(Some(p), true, None), "38north 444141 3684706");
        assert_eq!(CoordReadout::Utm.readout(Some(Coord::new(-58.4, -34.6)), true, None), "21south 371625 6170423");
        assert_eq!(CoordReadout::Mgrs.readout(Some(p), true, None), "38S MB 44140 84706");
        for f in CoordReadout::ALL {
            for earth in [true, false] {
                assert_eq!(f.readout(None, earth, None), "—");
                assert_eq!(f.readout(Some(Coord::new(f64::NAN, 0.0)), earth, None), "—");
            }
        }
    }

    #[test]
    fn polar_places_read_in_ups() {
        let pole = Coord::new(10.0, 86.0);
        // GeographicLib's `GeoConvert -u -l` and `-m` for the same place.
        assert_eq!(CoordReadout::Utm.readout(Some(pole), true, None), "north 2077146 1562481");
        assert_eq!(CoordReadout::Mgrs.readout(Some(pole), true, None), "Z AC 77146 62481");
        for f in [CoordReadout::Utm, CoordReadout::Mgrs] {
            assert_eq!(f.format(pole, true, None).as_deref(), Some(f.readout(Some(pole), true, None).as_str()));
            assert!(f.hover().contains("UPS"));
        }
        // Decimal and DMS read there as anywhere.
        assert_eq!(CoordReadout::Dms.readout(Some(pole), true, None), "10°00′00.0″ E  86°00′00.0″ N");
    }

    /// On another body's map UTM and MGRS, the Earth's grids, read nothing; degrees still do.
    #[test]
    fn grids_are_the_earths() {
        let p = Coord::new(44.4, 33.3);
        for f in [CoordReadout::Utm, CoordReadout::Mgrs] {
            assert_eq!(f.format(p, false, None), None);
            let r = f.readout(Some(p), false, None);
            assert_eq!(r, format!("No {} here", f.label()));
            // No wider than the reference it stands for, in the status bar.
            assert!(r.chars().count() <= f.readout(Some(p), true, None).chars().count(), "{r}");
            assert!(f.earth_only());
        }
        for f in [CoordReadout::Map, CoordReadout::Decimal, CoordReadout::Dms] {
            assert_eq!(f.format(p, false, None), f.format(p, true, None));
            assert!(!f.earth_only());
        }
    }

    /// The map's coordinates: Web Mercator's metres, an EPSG system's own units (Swiss LV95's
    /// metres, a US State Plane system's feet), decimals as a pixel needs them, and degrees
    /// without a flat Earth map.
    #[test]
    fn map_coordinates_read_in_the_maps_system() {
        let bern = Coord::new(7.4386, 46.9511);
        let lv95 = MapSystem::new(Flat::of(&rustgis_engine::map::Projection::Custom("EPSG:2056".into())), 2.0, "EPSG:2056");
        let r = CoordReadout::Map.readout(Some(bern), true, Some(&lv95));
        let (e, n) = r.split_once("  ").expect("an easting and a northing");
        let e: f64 = e.strip_suffix(" mE").expect("metres east").parse().expect("a number east");
        let n: f64 = n.strip_suffix(" mN").expect("metres north").parse().expect("a number north");
        // Bern's old observatory is the origin of the Swiss grid: E 2600000, N 1200000.
        assert!((e - 2_600_000.0).abs() < 50.0 && (n - 1_200_000.0).abs() < 50.0, "{r}");
        assert!(!r.contains('.'), "{r}");
        // Zoomed in to 5 cm a pixel, two decimals.
        let close = MapSystem::new(Flat::of(&rustgis_engine::map::Projection::Custom("EPSG:2056".into())), 0.05, "EPSG:2056");
        let r = CoordReadout::Map.readout(Some(bern), true, Some(&close));
        let decimals = |part: &str| part.split_once('.').map(|(_, d)| d.trim_end_matches([' ', 'm', 'E', 'N']).len());
        assert!(r.split("  ").all(|part| decimals(part) == Some(2)), "{r}");
        // Web Mercator: the origin, and a world copy past the antimeridian.
        let merc = MapSystem::new(Flat::Mercator, 1000.0, "Web Mercator");
        assert_eq!(CoordReadout::Map.readout(Some(Coord::new(0.0, 0.0)), true, Some(&merc)), "0 mE  0 mN");
        assert_eq!(
            CoordReadout::Map.readout(Some(Coord::new(370.0, 0.0)), true, Some(&merc)),
            CoordReadout::Map.readout(Some(Coord::new(10.0, 0.0)), true, Some(&merc))
        );
        // NAD83 / Texas Central (ftUS), EPSG:2277.
        let texas = MapSystem::new(Flat::of(&rustgis_engine::map::Projection::Custom("EPSG:2277".into())), 10.0, "EPSG:2277");
        let r = CoordReadout::Map.readout(Some(Coord::new(-97.74, 30.27)), true, Some(&texas));
        assert!(r.ends_with(" ftUSN") && r.contains(" ftUSE  "), "{r}");
        // Outside the system's reach: said, not a wrong number.
        let far = CoordReadout::Map.readout(Some(Coord::new(-150.0, -60.0)), true, Some(&lv95));
        assert_eq!(far, "No map coordinates here");
        // Its projection reaches well past Switzerland: London still has eastings and northings.
        let london = CoordReadout::Map.readout(Some(Coord::new(-0.1276, 51.5072)), true, Some(&lv95));
        assert!(london.ends_with(" mN"), "{london}");
        // The globe and another body's map have no flat Earth system: degrees.
        assert_eq!(CoordReadout::Map.readout(Some(bern), true, None), geom::format_lonlat(bern));
        assert!(CoordReadout::Map.hover().contains("map's coordinate system"));
    }

    /// The copied name gives the system back: a code, never a preset's or a local view's
    /// display name.
    #[test]
    fn the_system_is_named_so_it_can_be_given_back() {
        use rustgis_engine::map::Projection;
        assert_eq!(system_name(&Projection::WebMercator), "Web Mercator");
        assert_eq!(system_name(&Projection::from_code("EPSG:2056").expect("LV95")), "EPSG:2056");
        for code in ["robinson +lon_0=150", "local:7.4386,46.9511"] {
            let p = Projection::from_code(code).expect(code);
            let name = system_name(&p);
            assert_eq!(Projection::from_code(&name).expect(&name), p, "{name}");
        }
    }
}
